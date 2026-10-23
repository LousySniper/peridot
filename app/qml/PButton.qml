import QtQuick
import QtQuick.Controls.Basic

// Buttons: `primary` fills with the accent, `quiet` has no frame.
Button {
    id: b
    property bool primary: false
    property bool quiet: false
    property bool destructive: false

    implicitHeight: 32
    leftPadding: 14
    rightPadding: 14
    font.pixelSize: Look.body

    contentItem: Text {
        text: b.text
        font: b.font
        color: !b.enabled ? Look.faint
             : b.primary ? Look.onAccent
             : b.destructive ? Look.danger
             : Look.text
        horizontalAlignment: Text.AlignHCenter
        verticalAlignment: Text.AlignVCenter
    }
    background: Rectangle {
        radius: Look.radius
        color: b.primary ? (b.down ? Qt.darker(Look.accent, 1.15) : Look.accent)
             : b.quiet ? (b.hovered || b.down ? Look.raised : "transparent")
             : (b.down ? Look.raised : b.hovered ? Look.raised : Look.surface)
        border.color: b.visualFocus ? Look.focus : (b.primary || b.quiet ? "transparent" : Look.line)
        border.width: b.visualFocus ? 2 : 1
        opacity: b.enabled ? 1 : 0.6
    }
}
