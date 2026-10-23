import QtQuick
import QtQuick.Controls.Basic

// Slider that follows the device value except while the user drags it, and
// reports the final value once (on release, or after keyboard steps settle).
Slider {
    id: root
    property real deviceValue: 0
    signal committed(real value)

    implicitWidth: 260
    live: true

    Binding on value {
        when: !root.pressed && !settle.running && !hold.running
        value: root.deviceValue
        restoreMode: Binding.RestoreNone
    }

    onPressedChanged: if (!pressed) { hold.restart(); committed(value) }
    onDeviceValueChanged: hold.stop()
    onMoved: if (!pressed) settle.restart()

    Timer {
        id: settle
        interval: 350
        onTriggered: { hold.restart(); root.committed(root.value) }
    }

    // Keep showing the chosen value until the device confirms it.
    Timer { id: hold; interval: 2000 }

    background: Item {
        x: root.leftPadding
        y: root.topPadding + root.availableHeight / 2 - 2
        width: root.availableWidth
        height: 4
        Rectangle { anchors.fill: parent; radius: 2; color: Look.line }
        Rectangle {
            width: root.visualPosition * parent.width
            height: parent.height
            radius: 2
            color: root.enabled ? Look.accent : Look.faint
        }
    }

    handle: Rectangle {
        x: root.leftPadding + root.visualPosition * (root.availableWidth - width)
        y: root.topPadding + root.availableHeight / 2 - height / 2
        width: 18
        height: 18
        radius: 9
        color: Look.surface
        border.color: root.activeFocus ? Look.focus : (root.pressed ? Look.accent : Look.line)
        border.width: root.activeFocus ? 3 : 2
    }
}
