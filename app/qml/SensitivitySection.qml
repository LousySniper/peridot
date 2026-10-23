import QtQuick
import QtQuick.Controls.Basic
import QtQuick.Layouts

Column {
    id: root
    property var device: ({})
    readonly property var dpi: device.dpi
    readonly property var polling: device.polling_rate
    readonly property string id_: device.id || ""

    spacing: 20

    // working copy of the stages, reset whenever the device reports new ones
    property var stages: dpi ? dpi.stages : []
    property int active: dpi ? dpi.active_stage : 0
    Connections {
        target: root
        function onDpiChanged() {
            if (root.dpi) { root.stages = root.dpi.stages; root.active = root.dpi.active_stage }
        }
    }

    function push(stages, active) {
        root.stages = stages
        root.active = Math.max(0, active)
        Daemon.setSetting(id_, "dpi_stages", { stages: stages, active: root.active })
    }

    SectionTitle {
        visible: !!root.dpi
        text: qsTr("DPI stages")
        hint: root.dpi && root.dpi.hardware_stages
              ? qsTr("Your mouse's DPI button steps through these stages. Drag a marker to change it, click one to use it now, double-click the track to add one.")
              : qsTr("Stages you can step through with a button mapped to “DPI up/down”. Drag a marker to change it, click one to use it now, double-click the track to add one.")
    }

    DpiRuler {
        visible: !!root.dpi
        width: parent.width
        dpi: root.dpi || ({})
        stages: root.stages
        active: root.active
        onStagesEdited: (s, a) => root.push(s, a)
        onActivated: (i) => root.push(root.stages, i)
    }

    RowLayout {
        visible: !!root.dpi
        width: parent.width
        spacing: 12

        Text {
            text: qsTr("Stage %1 of %2").arg(root.active + 1).arg(root.stages.length)
            color: Look.muted
            font.pixelSize: Look.body
        }
        SpinBox {
            id: exact
            from: root.dpi ? root.dpi.min : 100
            to: root.dpi ? root.dpi.max : 16000
            stepSize: root.dpi && root.dpi.step > 1 ? root.dpi.step : 50
            editable: true
            value: root.stages[root.active] || 0
            implicitWidth: 150
            implicitHeight: 34
            Accessible.name: qsTr("DPI of the current stage")
            textFromValue: (v) => Number(v).toLocaleString(Qt.locale(), "f", 0)
            valueFromText: (t) => Number.fromLocaleString(Qt.locale(), t)
            onValueModified: commit.restart()
            Timer {
                id: commit
                interval: 500
                onTriggered: {
                    const next = root.stages.slice()
                    next[root.active] = exact.value
                    root.push(next, root.active)
                }
            }
            up.indicator: Rectangle {
                x: exact.width - width
                height: exact.height; width: 34
                radius: Look.radius
                color: exact.up.pressed ? Look.raised : Look.surface
                border.color: Look.line
                Text { anchors.centerIn: parent; text: "+"; color: Look.text; font.pixelSize: Look.sub }
            }
            down.indicator: Rectangle {
                height: exact.height; width: 34
                radius: Look.radius
                color: exact.down.pressed ? Look.raised : Look.surface
                border.color: Look.line
                Text { anchors.centerIn: parent; text: "\u2212"; color: Look.text; font.pixelSize: Look.sub }
            }
            background: Rectangle {
                radius: Look.radius
                color: Look.surface
                border.color: exact.activeFocus ? Look.focus : Look.line
                border.width: exact.activeFocus ? 2 : 1
            }
            contentItem: TextInput {
                text: exact.textFromValue(exact.value, exact.locale)
                color: Look.text
                font.pixelSize: Look.body
                font.features: { "tnum": 1 }
                horizontalAlignment: Qt.AlignHCenter
                verticalAlignment: Qt.AlignVCenter
                readOnly: !exact.editable
                validator: exact.validator
                inputMethodHints: Qt.ImhDigitsOnly
                selectByMouse: true
            }
        }
        Item { Layout.fillWidth: true }
        PButton {
            text: qsTr("Remove stage")
            enabled: root.stages.length > 1
            quiet: true
            onClicked: {
                const next = root.stages.slice()
                next.splice(root.active, 1)
                root.push(next, Math.min(root.active, next.length - 1))
            }
        }
        PButton {
            text: qsTr("Add stage")
            enabled: root.dpi && root.stages.length < (root.dpi.max_stages || 5)
            onClicked: {
                const top = root.stages[root.stages.length - 1] || 800
                const v = Math.min(root.dpi.max, top * 2)
                if (root.stages.indexOf(v) >= 0) return
                root.push(root.stages.concat([v]), root.stages.length)
            }
        }
    }

    Text {
        visible: root.device.onboard_mode === "onboard"
        width: parent.width
        wrapMode: Text.Wrap
        text: qsTr("The mouse is using the profile stored in it. Changing DPI or polling rate here switches it to Peridot's settings; you can switch back under Power.")
        color: Look.warn
        font.pixelSize: Look.small
    }

    Rectangle { visible: !!root.dpi && !!root.polling; width: parent.width; height: 1; color: Look.line }

    SettingRow {
        visible: !!root.polling
        label: qsTr("Polling rate")
        hint: qsTr("How often the mouse reports its position. Higher feels smoother and uses more battery.")
        Segmented {
            options: root.polling ? root.polling.options.map(v => ({ value: v, label: v >= 1000 ? (v / 1000) + " kHz" : v + " Hz" })) : []
            current: root.polling ? root.polling.value : 0
            onPicked: (v) => Daemon.setSetting(root.id_, "polling_rate", v)
        }
    }
}
