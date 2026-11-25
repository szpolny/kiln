//! The child process of one turn: its output as events, and a way to stop it.

use crate::AgentEvent;
use std::io::{self, BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

/// How long a process asked to stop may take before it is killed.
const STOP_GRACE: Duration = Duration::from_secs(5);

/// Handle to a turn's process. It owns the child it was spawned with, so it can
/// never signal any other process.
#[derive(Clone)]
pub struct Process {
    state: Arc<Mutex<State>>,
    grace: Duration,
}

struct State {
    child: Child,
    stop_requested: bool,
}

impl Process {
    /// Ask the process to exit, and kill it if it is still running after a grace
    /// period. Does nothing once it has exited.
    pub fn stop(&self) {
        let mut state = self.lock();
        // Only a child that has not been reaped is sure to still own its id.
        if state.stop_requested || !matches!(state.child.try_wait(), Ok(None)) {
            return;
        }
        state.stop_requested = true;
        // Terminating lets the agent stop the commands it started; killing it
        // outright would leave them running.
        // SAFETY: `kill` takes no pointers, and the id is this child's (above).
        unsafe { libc::kill(state.child.id() as libc::pid_t, libc::SIGTERM) };
        drop(state);
        let process = self.clone();
        thread::spawn(move || {
            thread::sleep(process.grace);
            // Does nothing if the child has been reaped by now.
            let _ = process.lock().child.kill();
        });
    }

    pub fn stop_requested(&self) -> bool {
        self.lock().stop_requested
    }

    /// Wait for the exit, leaving the process free to be stopped meanwhile.
    fn wait(&self) {
        while matches!(self.lock().child.try_wait(), Ok(None)) {
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // The state stays valid even if a holder panicked.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Spawn `cmd` and turn each line it prints into events with `parse`.
///
/// The last event is `Done`, or `Stopped` once a stopped process has exited, and
/// the channel then closes. Dropping the receiver stops the process.
pub(crate) fn spawn(
    cmd: Command,
    parse: fn(&str) -> Vec<AgentEvent>,
) -> io::Result<(Process, async_channel::Receiver<AgentEvent>)> {
    spawn_with_grace(cmd, parse, STOP_GRACE)
}

fn spawn_with_grace(
    mut cmd: Command,
    parse: fn(&str) -> Vec<AgentEvent>,
    grace: Duration,
) -> io::Result<(Process, async_channel::Receiver<AgentEvent>)> {
    let name = cmd.get_program().to_string_lossy().into_owned();
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn()?;
    let out = child.stdout.take();
    let state = State { child, stop_requested: false };
    let process = Process { state: Arc::new(Mutex::new(state)), grace };
    let (tx, rx) = async_channel::unbounded();
    let reader = process.clone();
    thread::spawn(move || {
        let mut finished = false;
        let lines = out.into_iter().flat_map(|out| BufReader::new(out).lines());
        'read: for line in lines.map_while(Result::ok) {
            for ev in parse(&line) {
                let outcome = matches!(ev, AgentEvent::Done { .. } | AgentEvent::Denied { .. });
                // A stopped turn has no outcome to act on, only what already happened.
                if outcome && reader.stop_requested() {
                    continue;
                }
                finished |= matches!(ev, AgentEvent::Done { .. });
                if tx.send_blocking(ev).is_err() {
                    reader.stop();
                    break 'read;
                }
            }
        }
        reader.wait();
        if !finished {
            let last = if reader.stop_requested() {
                AgentEvent::Stopped
            } else {
                AgentEvent::Done { error: Some(format!("{name} exited without a result")) }
            };
            let _ = tx.send_blocking(last);
        }
    });
    Ok((process, rx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// Every line is a text event, except `done`.
    fn parse(line: &str) -> Vec<AgentEvent> {
        match line {
            "done" => vec![AgentEvent::Done { error: None }],
            text => vec![AgentEvent::Text(text.into())],
        }
    }

    fn shell(script: &str, grace: Duration) -> (Process, async_channel::Receiver<AgentEvent>) {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", script]);
        spawn_with_grace(cmd, parse, grace).unwrap()
    }

    fn rest(rx: &async_channel::Receiver<AgentEvent>) -> Vec<AgentEvent> {
        std::iter::from_fn(|| rx.recv_blocking().ok()).collect()
    }

    fn exited(process: &Process) -> bool {
        matches!(process.lock().child.try_wait(), Ok(Some(_)))
    }

    #[test]
    fn stopping_ends_the_process_and_reports_it() {
        let grace = Duration::from_secs(30);
        let (process, rx) = shell("echo ready; exec sleep 60", grace);
        assert_eq!(rx.recv_blocking(), Ok(AgentEvent::Text("ready".into())));
        let start = Instant::now();
        process.stop();
        assert_eq!(rest(&rx), vec![AgentEvent::Stopped]);
        assert!(start.elapsed() < grace, "it should exit when asked to");
        assert!(exited(&process));
    }

    #[test]
    fn a_process_that_ignores_the_request_is_killed() {
        let script = "trap '' TERM; echo ready; exec sleep 60";
        let (process, rx) = shell(script, Duration::from_millis(200));
        assert_eq!(rx.recv_blocking(), Ok(AgentEvent::Text("ready".into())));
        process.stop();
        assert_eq!(rest(&rx), vec![AgentEvent::Stopped]);
        assert!(exited(&process));
    }

    #[test]
    fn a_result_printed_while_stopping_is_dropped() {
        let script = "trap 'echo bye; echo done; exit' TERM; echo ready; \
                      while :; do sleep 0.05; done";
        let (process, rx) = shell(script, Duration::from_secs(30));
        assert_eq!(rx.recv_blocking(), Ok(AgentEvent::Text("ready".into())));
        process.stop();
        assert_eq!(rest(&rx), vec![AgentEvent::Text("bye".into()), AgentEvent::Stopped]);
    }

    #[test]
    fn stopping_after_the_exit_changes_nothing() {
        let (process, rx) = shell("echo done", Duration::from_secs(30));
        assert_eq!(rest(&rx), vec![AgentEvent::Done { error: None }]);
        process.stop();
        assert!(!process.stop_requested());
    }

    #[test]
    fn exiting_without_a_result_is_an_error() {
        let (_process, rx) = shell("exit 1", Duration::from_secs(30));
        let error = Some("sh exited without a result".into());
        assert_eq!(rest(&rx), vec![AgentEvent::Done { error }]);
    }

    #[test]
    fn dropping_the_receiver_stops_the_process() {
        let script = "while :; do echo tick; sleep 0.02; done";
        let (process, rx) = shell(script, Duration::from_secs(30));
        drop(rx);
        let start = Instant::now();
        while !exited(&process) {
            assert!(start.elapsed() < Duration::from_secs(10), "still running");
            thread::sleep(Duration::from_millis(10));
        }
    }
}
