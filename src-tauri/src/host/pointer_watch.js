// Tells Dusk when the focused window takes the pointer, so a stream can send
// relative motion instead of absolute positions a locked pointer ignores.
//
// Loaded into KWin by Dusk over org.kde.kwin.Scripting, and unloaded when
// Dusk stops. It reads window state and nothing else: no window is moved,
// activated, resized or closed from here.
//
// # What it sends, and what it does not
//
// One string per change: "1:<class>" when the active window is fullscreen,
// "0:<class>" when it is not. A single string argument rather than a string
// and a bool because that is the shape proven to marshal through callDBus on
// Plasma 6, and the parsing it costs on the other side is three lines.
//
// The window's caption is never read. A title is the one field here that
// would be worth keeping private, and the decision does not need it.
//
// # Why it reports changes rather than events
//
// Measured on this host, one game launch produced six window events, four of
// which were focus moving between windows that were not fullscreen. Sending
// all of them would make the client's pointer mode a function of whether
// Discord happened to take focus. So the last reported pair is remembered and
// nothing is sent until it differs.
//
// Dusk decides what a class means; this only reports. The deny-list that
// separates a fullscreen game from a fullscreen video lives in Rust where it
// can be tested, rather than here where it cannot.

var DUSK_SERVICE = "dev.dusk.app.PointerWatch";
var DUSK_PATH = "/dev/dusk/app/PointerWatch";
var DUSK_IFACE = "dev.dusk.app.PointerWatch";

var lastSent = null;

function send(payload) {
    if (payload === lastSent) {
        return;
    }
    lastSent = payload;
    try {
        callDBus(DUSK_SERVICE, DUSK_PATH, DUSK_IFACE, "Report", payload);
    } catch (e) {
        // Dusk is not listening, which is the ordinary state when Dusk has
        // stopped but KWin has not yet unloaded this. Forget what was sent so
        // the next change is reported in full once it comes back.
        lastSent = null;
    }
}

function reportActive() {
    var w = null;
    try {
        w = workspace.activeWindow;
    } catch (e) {
        w = null;
    }

    if (!w) {
        // Nothing focused: the desktop itself, or a transition. Treated as
        // nothing holding the pointer, which is the safe direction -- it
        // hands the pointer back rather than taking it away.
        send("0:");
        return;
    }

    var cls = "";
    try {
        cls = w.resourceClass ? String(w.resourceClass) : "";
    } catch (e) {
        cls = "";
    }

    var fs = false;
    try {
        fs = w.fullScreen === true;
    } catch (e) {
        fs = false;
    }

    // The class is only sent for a fullscreen window, because it is only
    // read for one: nothing that is not fullscreen takes the pointer, so
    // Dusk's rule returns the same answer whatever the class says. Sending
    // it anyway made every focus change between ordinary windows a report --
    // measured on this host, four in a row for tabbing between SAI,
    // Telegram and Discord, none of which meant anything. Collapsing them to
    // one bare "0:" also keeps a class Dusk will not look at off the wire.
    send(fs ? "1:" + cls : "0:");
}

// A window can take the pointer two ways: by gaining focus while already
// fullscreen (what a game launch looks like -- measured, the activated event
// already carried fullScreen=true), or by going fullscreen while it already
// has focus (alt-enter in a running game). Both are hooked, and both end in
// the same change check.
function watch(w) {
    if (!w) {
        return;
    }
    try {
        if (w.fullScreenChanged) {
            w.fullScreenChanged.connect(reportActive);
        }
    } catch (e) {
        // A window type without the signal. Focus changes still cover it.
    }
}

try {
    workspace.windowActivated.connect(reportActive);
} catch (e) {
    // Without this there is no point continuing; Dusk will see no reports and
    // fall back to the mode the stream was launched with.
}

try {
    workspace.windowAdded.connect(watch);
    var existing = workspace.windowList ? workspace.windowList() : [];
    for (var i = 0; i < existing.length; i++) {
        watch(existing[i]);
    }
} catch (e) {
    // Focus changes alone still work.
}

// The state at load time, so a stream started while a game is already
// fullscreen is right from the first moment rather than from the first
// window change.
reportActive();
