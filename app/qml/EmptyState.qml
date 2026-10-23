import QtQuick
import QtQuick.Layouts

// Shown when the service isn't running or nothing is connected.
Item {
    id: root
    property string title: ""
    property string body: ""
    property string actionText: ""
    property string kind: "mouse"
    signal action()

    ColumnLayout {
        anchors.centerIn: parent
        width: Math.min(parent.width - 64, 440)
        spacing: 14

        DeviceGlyph {
            Layout.alignment: Qt.AlignHCenter
            kind: root.kind
            size: 56
            color: Look.faint
        }
        Text {
            Layout.fillWidth: true
            horizontalAlignment: Text.AlignHCenter
            wrapMode: Text.Wrap
            text: root.title
            color: Look.text
            font.pixelSize: Look.title - 4
            font.weight: Font.DemiBold
        }
        Text {
            Layout.fillWidth: true
            horizontalAlignment: Text.AlignHCenter
            wrapMode: Text.Wrap
            text: root.body
            color: Look.muted
            font.pixelSize: Look.body
            lineHeight: 1.25
        }
        PButton {
            visible: root.actionText.length > 0
            Layout.alignment: Qt.AlignHCenter
            Layout.topMargin: 6
            text: root.actionText
            primary: true
            onClicked: root.action()
        }
    }
}
