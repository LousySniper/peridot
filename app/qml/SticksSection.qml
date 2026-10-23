import QtQuick
import QtQuick.Controls.Basic
import QtQuick.Layouts

// Controllers: the stick-drift fix.
Column {
    id: root
    property var device: ({})
    readonly property var sticks: device.sticks || ({})
    readonly property string devId: device.id || ""
    property bool calibrating: false

    spacing: 20

    Connections {
        target: Daemon
        function onSettingSaved(id, key) { if (id === root.devId && key === "calibrate_sticks") root.calibrating = false }
        function onSettingFailed(id, key) { if (id === root.devId && key === "calibrate_sticks") root.calibrating = false }
    }

    SectionTitle {
        width: parent.width
        text: qsTr("Stick drift")
        hint: qsTr("When a stick moves on its own, Peridot can give games a corrected copy of the controller: the sticks re-centred and a dead zone ignoring the drift. Vibration still works; the touchpad and motion sensors stay on the original controller.")
    }

    SettingRow {
        label: qsTr("Fix stick drift")
        hint: qsTr("Steam and some games read the controller directly and won't see the fix; in Steam, use the dead zone in the controller settings instead.")
        PSwitch {
            checked: !!root.sticks.enabled
            onToggled: Daemon.setSetting(root.devId, "sticks", { enabled: checked })
        }
    }

    Text {
        visible: !!root.sticks.error
        width: parent.width
        text: root.sticks.error || ""
        color: Look.warn
        font.pixelSize: Look.small
        wrapMode: Text.Wrap
    }

    SettingRow {
        label: qsTr("Calibrate")
        hint: qsTr("Put the controller down and don't touch the sticks for two seconds. Peridot measures where they rest, sets dead zones just above the drift and turns the fix on.")
        PButton {
            text: root.calibrating ? qsTr("Measuring…") : qsTr("Calibrate")
            enabled: !root.calibrating
            onClicked: {
                root.calibrating = true
                Daemon.setSetting(root.devId, "calibrate_sticks", true)
            }
        }
    }

    Repeater {
        model: [
            { key: "left", label: qsTr("Left stick dead zone") },
            { key: "right", label: qsTr("Right stick dead zone") }
        ].filter(s => !!root.sticks[s.key])
        delegate: SettingRow {
            required property var modelData
            readonly property var stick: root.sticks[modelData.key] || ({})
            label: modelData.label
            hint: stick.center ? qsTr("How far the stick moves before games see it. Calibrated.")
                               : qsTr("How far the stick moves before games see it.")
            value: slider.value.toFixed(0) + "%"
            PSlider {
                id: slider
                from: 0; to: 30; stepSize: 1
                deviceValue: stick.deadzone || 0
                onCommitted: (v) => {
                    const change = {}
                    change[modelData.key] = { deadzone: Math.round(v) }
                    Daemon.setSetting(root.devId, "sticks", change)
                }
            }
        }
    }
}
