import QtQuick
import QtQuick.Layouts

Column {
    id: root
    property var device: ({})
    signal forgetRequested()

    spacing: 20

    readonly property var facts: [
        [qsTr("Model"), device.model],
        [qsTr("Connected through"), device.via],
        [qsTr("Firmware"), device.firmware],
        [qsTr("Serial number"), device.serial],
        [qsTr("Protocol"), device.protocol],
        [qsTr("Device id"), device.id]
    ].filter(f => f[1])

    GridLayout {
        width: parent.width
        columns: 2
        columnSpacing: 24
        rowSpacing: 10
        Repeater {
            model: root.facts
            delegate: Text {
                required property var modelData
                required property int index
                Layout.row: index
                Layout.column: 0
                text: modelData[0]
                color: Look.muted
                font.pixelSize: Look.body
            }
        }
        Repeater {
            model: root.facts
            delegate: Text {
                required property var modelData
                required property int index
                Layout.row: index
                Layout.column: 1
                Layout.fillWidth: true
                text: modelData[1]
                color: Look.text
                font.pixelSize: Look.body
                textFormat: Text.PlainText
                elide: Text.ElideRight
            }
        }
    }

    Repeater {
        model: root.device.notes || []
        delegate: Text {
            required property string modelData
            width: root.width
            wrapMode: Text.Wrap
            text: modelData
            color: Look.muted
            font.pixelSize: Look.body
        }
    }

    Rectangle { width: parent.width; height: 1; color: Look.line }

    SettingRow {
        label: qsTr("Saved settings")
        hint: qsTr("Peridot re-applies your settings every time this device connects or wakes up.")
        Row {
            spacing: 8
            PButton {
                text: qsTr("Check battery now")
                visible: !!root.device.battery
                onClicked: Daemon.refresh(root.device.id)
            }
            PButton {
                text: qsTr("Forget settings")
                destructive: true
                onClicked: root.forgetRequested()
            }
        }
    }
}
