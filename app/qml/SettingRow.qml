import QtQuick
import QtQuick.Layouts

// Label (and optional explanation) on the left, a control on the right.
RowLayout {
    id: row
    property string label: ""
    property string hint: ""
    property string value: ""
    default property alias control: holder.data
    width: parent ? parent.width : implicitWidth
    spacing: 16

    Column {
        Layout.fillWidth: true
        Layout.alignment: Qt.AlignVCenter
        spacing: 2
        Text {
            text: row.label
            color: Look.text
            font.pixelSize: Look.body
            width: parent.width
            elide: Text.ElideRight
        }
        Text {
            visible: row.hint.length > 0
            text: row.hint
            color: Look.muted
            font.pixelSize: Look.small
            width: parent.width
            wrapMode: Text.Wrap
        }
    }
    Text {
        visible: row.value.length > 0
        text: row.value
        color: Look.muted
        font.pixelSize: Look.body
        font.features: { "tnum": 1 }
        Layout.alignment: Qt.AlignVCenter
    }
    Item {
        id: holder
        Layout.alignment: Qt.AlignVCenter
        implicitWidth: childrenRect.width
        implicitHeight: childrenRect.height
    }
}
