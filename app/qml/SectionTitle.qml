import QtQuick

// A group heading inside a section, with optional one-line explanation.
Column {
    property alias text: heading.text
    property string hint: ""
    spacing: 2
    width: parent ? parent.width : implicitWidth
    topPadding: 8

    Text {
        id: heading
        color: Look.text
        font.pixelSize: Look.sub
        font.weight: Font.DemiBold
    }
    Text {
        visible: text.length > 0
        text: parent.hint
        width: parent.width
        wrapMode: Text.Wrap
        color: Look.muted
        font.pixelSize: Look.small
    }
}
