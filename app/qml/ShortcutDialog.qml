import QtQuick
import QtQuick.Controls.Basic
import QtQuick.Layouts

// Press the key combination a button should send.
Popup {
    id: dlg
    property string controlName: ""
    property string combo: ""
    signal accepted(string action)

    modal: true
    focus: true
    anchors.centerIn: Overlay.overlay
    width: 420
    padding: 24
    onOpened: { combo = ""; capture.forceActiveFocus() }

    background: Rectangle { radius: 10; color: Look.surface; border.color: Look.line }
    Overlay.modal: Rectangle { color: Qt.rgba(0, 0, 0, 0.45) }

    function keyName(e) {
        const named = {
            [Qt.Key_Escape]: "esc", [Qt.Key_Tab]: "tab", [Qt.Key_Backspace]: "backspace", [Qt.Key_Return]: "enter",
            [Qt.Key_Enter]: "enter", [Qt.Key_Insert]: "insert", [Qt.Key_Delete]: "delete", [Qt.Key_Pause]: "pause",
            [Qt.Key_Print]: "print", [Qt.Key_Home]: "home", [Qt.Key_End]: "end", [Qt.Key_Left]: "left",
            [Qt.Key_Up]: "up", [Qt.Key_Right]: "right", [Qt.Key_Down]: "down", [Qt.Key_PageUp]: "pageup",
            [Qt.Key_PageDown]: "pagedown", [Qt.Key_Space]: "space", [Qt.Key_Menu]: "menu",
            [Qt.Key_Minus]: "minus", [Qt.Key_Equal]: "equal", [Qt.Key_BracketLeft]: "leftbrace",
            [Qt.Key_BracketRight]: "rightbrace", [Qt.Key_Semicolon]: "semicolon", [Qt.Key_Apostrophe]: "apostrophe",
            [Qt.Key_QuoteLeft]: "grave", [Qt.Key_Backslash]: "backslash", [Qt.Key_Comma]: "comma",
            [Qt.Key_Period]: "dot", [Qt.Key_Slash]: "slash"
        }
        if (named[e.key]) return named[e.key]
        if (e.key >= Qt.Key_F1 && e.key <= Qt.Key_F24) return "f" + (e.key - Qt.Key_F1 + 1)
        if ((e.key >= Qt.Key_A && e.key <= Qt.Key_Z) || (e.key >= Qt.Key_0 && e.key <= Qt.Key_9))
            return String.fromCharCode(e.key).toLowerCase()
        return ""
    }

    ColumnLayout {
        width: parent.width
        spacing: 16

        Text {
            text: qsTr("Shortcut for %1").arg(dlg.controlName)
            color: Look.text
            font.pixelSize: Look.sub
            font.weight: Font.DemiBold
        }
        Text {
            Layout.fillWidth: true
            wrapMode: Text.Wrap
            text: qsTr("Press the keys together, for example Ctrl+Shift+T.")
            color: Look.muted
            font.pixelSize: Look.body
        }
        Rectangle {
            id: capture
            Layout.fillWidth: true
            implicitHeight: 56
            radius: Look.radius
            color: Look.window
            border.color: activeFocus ? Look.accent : Look.line
            border.width: 2
            focus: true
            Text {
                anchors.centerIn: parent
                text: dlg.combo ? dlg.combo.split("+").map(Look.sentence).join(" + ") : qsTr("Waiting for keys…")
                color: dlg.combo ? Look.text : Look.faint
                font.pixelSize: Look.sub
            }
            Keys.onPressed: (e) => {
                e.accepted = true
                const name = dlg.keyName(e)
                if (!name) return
                const mods = []
                if (e.modifiers & Qt.ControlModifier) mods.push("ctrl")
                if (e.modifiers & Qt.ShiftModifier) mods.push("shift")
                if (e.modifiers & Qt.AltModifier) mods.push("alt")
                if (e.modifiers & Qt.MetaModifier) mods.push("super")
                dlg.combo = mods.concat([name]).join("+")
            }
        }
        RowLayout {
            Layout.fillWidth: true
            Item { Layout.fillWidth: true }
            PButton { text: qsTr("Cancel"); quiet: true; onClicked: dlg.close() }
            PButton {
                text: qsTr("Use shortcut")
                primary: true
                enabled: dlg.combo.length > 0
                onClicked: { dlg.accepted("key:" + dlg.combo); dlg.close() }
            }
        }
    }
}
