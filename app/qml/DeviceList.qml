import QtQuick
import QtQuick.Controls.Basic

Rectangle {
    id: root
    property string currentId: ""
    signal chosen(string id)

    color: Look.sidebar

    function stateLine(d) {
        if (!d.online) return qsTr("Asleep")
        const b = d.battery
        if (b && b.percent !== null && b.percent !== undefined) {
            if (b.charging) return qsTr("%1% · charging").arg(b.percent)
            if (b.percent <= 10) return qsTr("%1% · charge soon").arg(b.percent)
            return b.percent + "%"
        }
        return d.connection === "receiver" ? qsTr("Wireless") : qsTr("Wired")
    }

    ListView {
        id: list
        anchors.fill: parent
        anchors.topMargin: 16
        model: Daemon.devices
        spacing: 2
        clip: true
        boundsBehavior: Flickable.StopAtBounds
        keyNavigationEnabled: true
        focus: true
        currentIndex: Daemon.devices.findIndex(d => d.id === root.currentId)
        Keys.onUpPressed: if (currentIndex > 0) root.chosen(Daemon.devices[currentIndex - 1].id)
        Keys.onDownPressed: if (currentIndex < count - 1) root.chosen(Daemon.devices[currentIndex + 1].id)

        delegate: Rectangle {
            id: row
            required property var modelData
            readonly property bool selected: modelData.id === root.currentId
            readonly property var bat: modelData.battery
            readonly property bool low: modelData.online && bat && bat.percent !== null && bat.percent <= 25 && !bat.charging
            width: ListView.view.width
            height: 60
            color: selected ? Look.window : hover.containsMouse ? Qt.rgba(1, 1, 1, Look.dark ? 0.03 : 0.4) : "transparent"

            Rectangle {
                visible: row.selected
                width: 3
                height: parent.height - 20
                anchors.verticalCenter: parent.verticalCenter
                radius: 1.5
                color: Look.accent
            }

            DeviceGlyph {
                id: glyph
                x: 18
                anchors.verticalCenter: parent.verticalCenter
                kind: row.modelData.kind
                size: 26
                color: row.modelData.online ? Look.text : Look.faint
            }
            Column {
                anchors.left: glyph.right
                anchors.leftMargin: 12
                anchors.right: parent.right
                anchors.rightMargin: 14
                anchors.verticalCenter: parent.verticalCenter
                spacing: 3
                Text {
                    width: parent.width
                    text: row.modelData.name
                    color: row.modelData.online ? Look.text : Look.muted
                    font.pixelSize: Look.body
                    font.weight: row.selected ? Font.DemiBold : Font.Normal
                    elide: Text.ElideRight
                }
                Text {
                    width: parent.width
                    text: root.stateLine(row.modelData)
                    color: row.low ? (row.bat.percent <= 10 ? Look.danger : Look.warn)
                         : (row.bat && row.bat.charging) ? Look.accent : Look.muted
                    font.pixelSize: Look.small
                    font.features: { "tnum": 1 }
                    elide: Text.ElideRight
                }
            }
            MouseArea {
                id: hover
                anchors.fill: parent
                hoverEnabled: true
                onClicked: { list.forceActiveFocus(); root.chosen(row.modelData.id) }
            }
        }
    }

}
