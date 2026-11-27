//! The on-demand dictionary catalog and its verified local cache.

use super::parse_dictionary;
use sha2::{Digest, Sha256};
use spellbook::Dictionary;
use std::{
    fs::{self, File},
    io::{self, Read, Write},
    path::Path,
    sync::Arc,
    time::Duration,
};

const UPSTREAM: &str = "https://raw.githubusercontent.com/LibreOffice/dictionaries";
const REVISION: &str = "79195b017e6d70151337856c59e7d13d311d6630";
const MAX_FILE_SIZE: u64 = 16 * 1024 * 1024;

pub struct Language {
    pub code: &'static str,
    pub name: &'static str,
    source: &'static str,
    aff_sha256: &'static str,
    dic_sha256: &'static str,
}

pub const LANGUAGES: &[Language] = &[
    Language {
        code: "en_GB",
        name: "English (UK)",
        source: "en/en_GB",
        aff_sha256: "0fd6ed120ef28957847d98ba5149b117e27116cf81b5aa36208453f6755a36fd",
        dic_sha256: "04e90f34f5263bf26780e9c4a442e9ad16584e227af49ddd1b3b21b01df5b29c",
    },
    Language {
        code: "fr_FR",
        name: "French",
        source: "fr_FR/dictionaries/fr",
        aff_sha256: "c176610cd5dc4846806a65ddd029f422d87978bf58f224aa44222662a16a2de5",
        dic_sha256: "b78a868e31dd6e373b6c3217969afb898a9acde828a5e7ef97308da42218c88c",
    },
    Language {
        code: "de_DE",
        name: "German",
        source: "de/de_DE_frami",
        aff_sha256: "646bf3333ac69c23e9d794533ee5241d6f755c359e8fe10a648f87613743d594",
        dic_sha256: "4ca3c958b0e5545910999bc246f668840bf8ede3df8e5e6790d05edd5a586c38",
    },
    Language {
        code: "it_IT",
        name: "Italian",
        source: "it_IT/it_IT",
        aff_sha256: "951afaa19272f13555b8823e8bcf9ccf78f8fe1a07835bdfb912ab3e4d537c2b",
        dic_sha256: "bae1e3501dcd2a923669592493b3fde6c02aae7c7aab83bf5e5b49077e73dd64",
    },
    Language {
        code: "pl_PL",
        name: "Polish",
        source: "pl_PL/pl_PL",
        aff_sha256: "82973651651aa930335c865b339b98db376ca3dbf3a661b70b9eeb71fdf41dca",
        dic_sha256: "c0848440599eb88e5aca500418d5f389e562ec2c157b63dbe39d354658ffba49",
    },
    Language {
        code: "pt_BR",
        name: "Portuguese (Brazil)",
        source: "pt_BR/pt_BR",
        aff_sha256: "21d8ad2a769a60e17e2b5ea4ef11d4d593a58b9e2a82d642ef82d6a4c5523865",
        dic_sha256: "a38bfb26b68ece2834e79fe83e48d5792652970ace12db89d1b9674bf9933183",
    },
    Language {
        code: "es_ES",
        name: "Spanish (Spain)",
        source: "es/es_ES",
        aff_sha256: "e73a9bf8e1383f4986a5dc9e2fbed49371c0c61f511c626d15586bd433c1cad9",
        dic_sha256: "6975dddec3d5d2c676069537bc67b4b5f786c65c5d4cf6703a82acf779ac9ec1",
    },
    Language {
        code: "uk_UA",
        name: "Ukrainian",
        source: "uk_UA/uk_UA",
        aff_sha256: "2219dd15e9802adebc45722c60943b1472640260491af38dd3e43b07e75585e6",
        dic_sha256: "2e5a9e67be63bdb089b3459addb5d71113319d13768e277bcae20f3cc1ad5a93",
    },
];

impl Language {
    pub fn cached(&self, cache: &Path) -> Option<Result<Arc<Dictionary>, String>> {
        let dir = cache.join(self.code);
        match dir.try_exists() {
            Ok(true) => Some(self.read_cached(&dir)),
            Ok(false) => None,
            Err(e) => Some(Err(format!("Could not find the saved dictionary. {e}"))),
        }
    }

    fn read_cached(&self, dir: &Path) -> Result<Arc<Dictionary>, String> {
        let read = |name, hash| {
            let mut bytes = Vec::new();
            File::open(dir.join(name))
                .and_then(|file| file.take(MAX_FILE_SIZE + 1).read_to_end(&mut bytes))
                .map_err(|e| format!("Could not read the saved dictionary. {e}"))?;
            verify(&bytes, hash)?;
            Ok::<_, String>(bytes)
        };
        let aff = read("dictionary.aff", self.aff_sha256)?;
        let dic = read("dictionary.dic", self.dic_sha256)?;
        parse_dictionary(&aff, &dic)
    }

    pub fn download(&self, cache: &Path) -> Result<Arc<Dictionary>, String> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .https_only(true)
            .max_redirects(0)
            .timeout_global(Some(Duration::from_secs(30)))
            .build()
            .into();
        let fetch = |extension, hash| {
            let url = format!("{UPSTREAM}/{REVISION}/{}.{extension}", self.source);
            let mut response = agent
                .get(&url)
                .call()
                .map_err(|e| format!("Could not download the dictionary from LibreOffice. {e}"))?;
            if response.status() != 200 {
                return Err(format!(
                    "Dictionary download returned HTTP {}.",
                    response.status()
                ));
            }
            let bytes = response
                .body_mut()
                .with_config()
                .limit(MAX_FILE_SIZE)
                .read_to_vec()
                .map_err(|e| format!("Could not read the dictionary download. {e}"))?;
            verify(&bytes, hash)?;
            Ok(bytes)
        };
        let aff = fetch("aff", self.aff_sha256)?;
        let dic = fetch("dic", self.dic_sha256)?;
        let dictionary = parse_dictionary(&aff, &dic)?;
        let dir = cache.join(self.code);
        let save = || -> io::Result<()> {
            fs::create_dir_all(&dir)?;
            save_file(&dir, "dictionary.aff", &aff)?;
            save_file(&dir, "dictionary.dic", &dic)
        };
        save().map_err(|e| format!("Could not save the dictionary. {e}"))?;
        Ok(dictionary)
    }
}

fn verify(bytes: &[u8], expected: &str) -> Result<(), String> {
    if bytes.len() as u64 > MAX_FILE_SIZE || format!("{:x}", Sha256::digest(bytes)) != expected {
        return Err("Dictionary files failed verification.".into());
    }
    Ok(())
}

fn save_file(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(dir)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(dir.join(name))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_dictionaries_require_both_verified_files() {
        let aff = b"SET UTF-8\n";
        let dic = b"1\nexample\n";
        let language = Language {
            code: "test",
            name: "Test",
            source: "",
            aff_sha256: "7f6d7c55043d4b09d0a4380720847457b7954048bf1dac70512593006bae8c37",
            dic_sha256: "304d64a6489e4794bb31656466b0b8fae892fb1b5f7081bb6cf6026d76f03192",
        };
        let cache = tempfile::tempdir().unwrap();
        assert!(language.cached(cache.path()).is_none());
        let dir = cache.path().join(language.code);
        fs::create_dir(&dir).unwrap();
        save_file(&dir, "dictionary.aff", aff).unwrap();
        assert!(language.cached(cache.path()).unwrap().is_err());
        save_file(&dir, "dictionary.dic", dic).unwrap();
        assert!(language
            .cached(cache.path())
            .unwrap()
            .unwrap()
            .check("example"));
        save_file(&dir, "dictionary.dic", b"1\nchanged\n").unwrap();
        assert!(language.cached(cache.path()).unwrap().is_err());
    }
}
