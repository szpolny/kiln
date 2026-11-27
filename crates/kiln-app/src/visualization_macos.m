#import <AppKit/AppKit.h>
#import <WebKit/WebKit.h>

// These WebKit controls turn off network hosts and media APIs, including WebRTC.
// Check them at runtime so a changed system WebKit fails closed.
@interface WKWebViewConfiguration (KilnNetworkPolicy)
- (void)_setAllowedNetworkHosts:(NSSet<NSString *> *)hosts;
@end

@interface WKPreferences (KilnNetworkPolicy)
- (void)_disableMediaPlaybackRelatedFeatures;
@end

typedef void (*KilnSnapshotCallback)(void *, const void *, size_t, const char *);

@interface KilnVisualizationWebView : WKWebView
@end

@implementation KilnVisualizationWebView
- (NSDragOperation)draggingEntered:(id<NSDraggingInfo>)sender { return NSDragOperationNone; }
- (NSDragOperation)draggingUpdated:(id<NSDraggingInfo>)sender { return NSDragOperationNone; }
- (BOOL)performDragOperation:(id<NSDraggingInfo>)sender { return NO; }
@end

@interface KilnVisualization : NSObject <WKNavigationDelegate, WKUIDelegate, NSWindowDelegate>
@property(nonatomic, strong) WKWebView *webView;
@property(nonatomic, strong) NSWindow *window;
@property(nonatomic) BOOL didStartDocument;
@property(nonatomic) BOOL didStartFrame;
@property(nonatomic) BOOL loaded;
@property(nonatomic, copy) void (^snapshot)(NSData *, NSString *);
@end

static NSMutableSet<KilnVisualization *> *activeVisualizations(void) {
    static NSMutableSet<KilnVisualization *> *sessions;
    if (!sessions) sessions = [NSMutableSet set];
    return sessions;
}

static NSString *escapedAttribute(NSString *value) {
    value = [value stringByReplacingOccurrencesOfString:@"&" withString:@"&amp;"];
    value = [value stringByReplacingOccurrencesOfString:@"\"" withString:@"&quot;"];
    return [value stringByReplacingOccurrencesOfString:@"<" withString:@"&lt;"];
}

static NSString *documentWithVisualization(NSString *html, BOOL dark) {
    NSString *policy = @"default-src 'none'; script-src 'unsafe-inline'; "
                       "style-src 'unsafe-inline'; img-src data: blob:; font-src data:; "
                       "base-uri 'none'; form-action 'none'; worker-src 'none'";
    NSString *child = [NSString stringWithFormat:
        @"<!doctype html><meta charset=utf-8><meta http-equiv=Content-Security-Policy content=\"%@; frame-src 'none'\">%@",
        policy, html];
    return [NSString stringWithFormat:
        @"<!doctype html><meta charset=utf-8>"
         "<meta name=viewport content=\"width=device-width, initial-scale=1\">"
         "<meta http-equiv=Content-Security-Policy content=\"%@; frame-src about:\">"
         "<style>:root{color-scheme:%@}html,body{margin:0;width:100%%;height:100%%}iframe{display:block;width:100%%;height:100%%;border:0}</style>"
         "<iframe sandbox=allow-scripts title=\"Visualization\" srcdoc=\"%@\"></iframe>",
        policy, dark ? @"dark" : @"light", escapedAttribute(child)];
}

@implementation KilnVisualization
- (void)finishSnapshot:(NSData *)png error:(NSString *)error {
    if (!self.snapshot) return;
    void (^callback)(NSData *, NSString *) = self.snapshot;
    self.snapshot = nil;
    self.webView.navigationDelegate = nil;
    [self.webView stopLoading];
    self.webView = nil;
    [activeVisualizations() removeObject:self];
    callback(png, error);
}

- (void)failed:(NSString *)error {
    if (self.snapshot) {
        [self finishSnapshot:nil error:error];
    } else if (self.window) {
        self.webView.navigationDelegate = nil;
        [self.webView stopLoading];
        self.webView = nil;
        NSView *content = [[NSView alloc] initWithFrame:self.window.contentView.bounds];
        NSTextField *label = [NSTextField wrappingLabelWithString:error];
        label.frame = NSInsetRect(content.bounds, 24, 24);
        label.autoresizingMask = NSViewWidthSizable | NSViewHeightSizable;
        [content addSubview:label];
        self.window.contentView = content;
    }
}

- (void)windowWillClose:(NSNotification *)notification {
    self.webView.navigationDelegate = nil;
    [self.webView stopLoading];
    self.webView = nil;
    self.window.delegate = nil;
    self.window = nil;
    [activeVisualizations() removeObject:self];
}

- (void)webView:(WKWebView *)view decidePolicyForNavigationAction:(WKNavigationAction *)action
    decisionHandler:(void (^)(WKNavigationActionPolicy))decide {
    NSString *url = action.request.URL.absoluteString;
    WKFrameInfo *target = action.targetFrame;
    if (target.isMainFrame && !self.didStartDocument && [url isEqualToString:@"about:blank"]) {
        self.didStartDocument = YES;
        decide(WKNavigationActionPolicyAllow);
    } else if (target && !target.isMainFrame && self.didStartDocument &&
               !self.didStartFrame && [url isEqualToString:@"about:srcdoc"]) {
        self.didStartFrame = YES;
        decide(WKNavigationActionPolicyAllow);
    } else {
        decide(WKNavigationActionPolicyCancel);
    }
}

- (void)webView:(WKWebView *)view runOpenPanelWithParameters:(WKOpenPanelParameters *)parameters
    initiatedByFrame:(WKFrameInfo *)frame completionHandler:(void (^)(NSArray<NSURL *> *))complete {
    complete(nil);
}

- (void)webView:(WKWebView *)view didFinishNavigation:(WKNavigation *)navigation {
    if (!self.didStartFrame) {
        [self failed:@"The isolated visualization frame could not load."];
        return;
    }
    self.loaded = YES;
    if (!self.snapshot) return;
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 150 * NSEC_PER_MSEC), dispatch_get_main_queue(), ^{
        WKSnapshotConfiguration *config = [[WKSnapshotConfiguration alloc] init];
        config.snapshotWidth = @640;
        [view takeSnapshotWithConfiguration:config completionHandler:^(NSImage *image, NSError *error) {
            CGImageRef bitmap = error ? NULL : [image CGImageForProposedRect:NULL context:nil hints:nil];
            NSData *png = bitmap ? [[[NSBitmapImageRep alloc] initWithCGImage:bitmap]
                representationUsingType:NSBitmapImageFileTypePNG properties:@{}] : nil;
            [self finishSnapshot:png error:png ? nil : @"The visualization preview could not be captured."];
        }];
    });
}

- (void)webView:(WKWebView *)view didFailNavigation:(WKNavigation *)navigation withError:(NSError *)error {
    [self failed:@"The visualization could not load."];
}

- (void)webView:(WKWebView *)view didFailProvisionalNavigation:(WKNavigation *)navigation withError:(NSError *)error {
    [self failed:@"The visualization could not load."];
}

- (void)webViewWebContentProcessDidTerminate:(WKWebView *)webView {
    [self failed:@"The visualization renderer stopped unexpectedly."];
}
@end

static BOOL startVisualization(NSString *html, NSString *title, BOOL dark, void (^snapshot)(NSData *, NSString *)) {
    WKWebViewConfiguration *config = [[WKWebViewConfiguration alloc] init];
    if (![config respondsToSelector:@selector(_setAllowedNetworkHosts:)] ||
        ![config.preferences respondsToSelector:@selector(_disableMediaPlaybackRelatedFeatures)]) {
        return NO;
    }
    [config _setAllowedNetworkHosts:[NSSet set]];
    [config.preferences _disableMediaPlaybackRelatedFeatures];
    config.websiteDataStore = [WKWebsiteDataStore nonPersistentDataStore];

    KilnVisualization *session = [[KilnVisualization alloc] init];
    session.snapshot = snapshot;
    NSRect frame = snapshot ? NSMakeRect(0, 0, 640, 360) : NSMakeRect(0, 0, 900, 560);
    session.webView = [[KilnVisualizationWebView alloc] initWithFrame:frame configuration:config];
    session.webView.appearance = [NSAppearance appearanceNamed:dark ? NSAppearanceNameDarkAqua : NSAppearanceNameAqua];
    session.webView.navigationDelegate = session;
    session.webView.UIDelegate = session;

    if (!snapshot) {
        NSWindow *window = [[NSWindow alloc] initWithContentRect:frame
            styleMask:NSWindowStyleMaskTitled | NSWindowStyleMaskClosable |
                      NSWindowStyleMaskMiniaturizable | NSWindowStyleMaskResizable
            backing:NSBackingStoreBuffered defer:NO];
        window.releasedWhenClosed = NO;
        window.appearance = [NSAppearance appearanceNamed:dark ? NSAppearanceNameDarkAqua : NSAppearanceNameAqua];
        window.title = title;
        window.contentView = session.webView;
        window.delegate = session;
        session.window = window;
        [window center];
        [window makeKeyAndOrderFront:nil];
        [window makeFirstResponder:session.webView];
    }
    [activeVisualizations() addObject:session];
    [session.webView loadHTMLString:documentWithVisualization(html, dark) baseURL:nil];
    __weak KilnVisualization *weakSession = session;
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 8 * NSEC_PER_SEC), dispatch_get_main_queue(), ^{
        KilnVisualization *session = weakSession;
        if (session.snapshot || !session.loaded) [session failed:@"The visualization timed out."];
    });
    return YES;
}

void kiln_visualization_snapshot(const uint8_t *html, size_t length, bool dark, void *context, KilnSnapshotCallback callback) {
    NSString *document = [[NSString alloc] initWithBytes:html length:length encoding:NSUTF8StringEncoding];
    dispatch_async(dispatch_get_main_queue(), ^{
        BOOL started = document && startVisualization(document, nil, dark, ^(NSData *png, NSString *error) {
            callback(context, png.bytes, png.length, error.UTF8String);
        });
        if (!started) callback(context, NULL, 0, "This system WebKit cannot isolate visualizations.");
    });
}

bool kiln_visualization_open(const uint8_t *html, size_t length, const uint8_t *title, size_t titleLength, bool dark) {
    NSString *document = [[NSString alloc] initWithBytes:html length:length encoding:NSUTF8StringEncoding];
    NSString *name = [[NSString alloc] initWithBytes:title length:titleLength encoding:NSUTF8StringEncoding];
    return document && name && startVisualization(document, name, dark, nil);
}
