pragma Singleton
import QtQuick
import QtCore

// Design tokens. Mineral slate surfaces, one peridot accent reserved for
// "active" (current stage, selected effect, charging). Follows the desktop's
// light/dark preference unless the user picked one (sidebar switch, or
// --color-scheme on the command line).
QtObject {
    /// "system", "light" or "dark"; remembered between runs.
    property string theme: store.theme
    onThemeChanged: store.theme = theme
    readonly property Settings store: Settings {
        category: "appearance"
        property string theme: "system"
    }
    readonly property string scheme: ForcedScheme === "light" || ForcedScheme === "dark" ? ForcedScheme : theme
    readonly property bool dark: scheme === "light" ? false
                               : scheme === "dark" ? true
                               : Application.styleHints.colorScheme !== Qt.ColorScheme.Light

    readonly property color window:   dark ? "#1e2530" : "#edf0ee"
    readonly property color sidebar:  dark ? "#19202a" : "#e2e7e4"
    readonly property color surface:  dark ? "#28313f" : "#ffffff"
    readonly property color raised:   dark ? "#313b4b" : "#f6f8f6"
    readonly property color line:     dark ? "#3a4555" : "#cbd3d0"
    readonly property color text:     dark ? "#e8ece6" : "#1f2833"
    readonly property color muted:    dark ? "#9aa5b1" : "#5b6873"
    readonly property color faint:    dark ? "#6c7886" : "#8b979f"
    readonly property color accent:   dark ? "#a3bb4a" : "#667d22"
    readonly property color accentSoft: dark ? Qt.rgba(0.64, 0.73, 0.29, 0.16) : Qt.rgba(0.40, 0.49, 0.13, 0.12)
    readonly property color onAccent: dark ? "#141a0a" : "#ffffff"
    readonly property color warn:     dark ? "#e3b04f" : "#a36b0b"
    readonly property color danger:   dark ? "#ec6b55" : "#b8392a"
    readonly property color focus:    dark ? "#c6dc6c" : "#4f6318"

    readonly property string family: Qt.application.font.family
    readonly property int small: 12
    readonly property int body: 14
    readonly property int sub: 16
    readonly property int title: 24
    readonly property int display: 34

    readonly property int radius: 6
    readonly property int gap: 8
    readonly property int pad: 24

    function batteryColor(pct, charging) {
        if (charging) return accent
        if (pct === undefined || pct === null) return muted
        if (pct <= 10) return danger
        if (pct <= 25) return warn
        return text
    }

    function sentence(s) {
        if (!s) return ""
        return s.charAt(0).toUpperCase() + s.slice(1).replace(/_/g, " ")
    }
}
