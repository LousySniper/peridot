import QtQuick
import QtQuick.Controls.Basic

// An on/off switch in Peridot's style.
Switch {
    id: control
    indicator: Rectangle {
        implicitWidth: 40; implicitHeight: 22; radius: 11
        color: control.checked ? Look.accent : Look.line
        border.color: control.visualFocus ? Look.focus : "transparent"
        border.width: 2
        Rectangle {
            x: control.checked ? parent.width - width - 3 : 3
            anchors.verticalCenter: parent.verticalCenter
            width: 16; height: 16; radius: 8
            color: Look.surface
            Behavior on x { NumberAnimation { duration: 120 } }
        }
    }
}
