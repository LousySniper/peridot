import QtQuick
import QtQuick.Controls.Basic

Column {
    id: root
    property var device: ({})
    readonly property var power: device.power || ({})
    readonly property string devId: device.id || ""

    spacing: 20

    function minutes(s) {
        const m = Math.round(s / 60)
        return m === 1 ? qsTr("1 minute") : qsTr("%1 minutes").arg(m)
    }

    SettingRow {
        visible: !!root.power.idle_time
        label: qsTr("Sleep after")
        hint: qsTr("The mouse goes to sleep after this long without use. Shorter saves battery.")
        value: root.minutes(idle.value)
        PSlider {
            id: idle
            from: root.power.idle_time ? root.power.idle_time.min : 60
            to: root.power.idle_time ? root.power.idle_time.max : 900
            stepSize: 60
            deviceValue: root.power.idle_time ? root.power.idle_time.value : 300
            onCommitted: (v) => Daemon.setSetting(root.devId, "idle_time", Math.round(v))
        }
    }

    SettingRow {
        visible: !!root.power.low_battery
        label: qsTr("Low battery warning")
        hint: qsTr("The mouse signals low battery when its charge drops below this.")
        value: low.value.toFixed(0) + "%"
        PSlider {
            id: low
            from: root.power.low_battery ? root.power.low_battery.min : 5
            to: root.power.low_battery ? root.power.low_battery.max : 25
            stepSize: 1
            deviceValue: root.power.low_battery ? root.power.low_battery.value : 10
            onCommitted: (v) => Daemon.setSetting(root.devId, "low_battery", Math.round(v))
        }
    }

    SettingRow {
        visible: !!root.device.onboard_mode
        label: qsTr("Use the profile stored in the mouse")
        hint: qsTr("When on, the mouse ignores Peridot's DPI and polling settings and uses what was saved to it with G HUB.")
        Switch {
            id: onboard
            checked: root.device.onboard_mode === "onboard"
            onToggled: Daemon.setSetting(root.devId, "onboard_mode", checked ? "onboard" : "host")
            indicator: Rectangle {
                implicitWidth: 40; implicitHeight: 22; radius: 11
                color: onboard.checked ? Look.accent : Look.line
                border.color: onboard.visualFocus ? Look.focus : "transparent"
                border.width: 2
                Rectangle {
                    x: onboard.checked ? parent.width - width - 3 : 3
                    anchors.verticalCenter: parent.verticalCenter
                    width: 16; height: 16; radius: 8
                    color: Look.surface
                    Behavior on x { NumberAnimation { duration: 120 } }
                }
            }
        }
    }
}
