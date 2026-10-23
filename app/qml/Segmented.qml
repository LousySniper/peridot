import QtQuick

// A row of mutually exclusive choices. `options` is a list of
// {value, label} objects or plain values.
Row {
    id: root
    property var options: []
    property var current
    property bool active: true
    signal picked(var value)

    spacing: 0

    Repeater {
        model: root.options
        delegate: Rectangle {
            id: seg
            required property var modelData
            required property int index
            readonly property var val: (modelData !== null && typeof modelData === "object") ? modelData.value : modelData
            readonly property string label: (modelData !== null && typeof modelData === "object") ? modelData.label : String(modelData)
            readonly property bool selected: root.current === val
            readonly property bool first: index === 0
            readonly property bool last: index === root.options.length - 1

            width: Math.max(56, caption.implicitWidth + 28)
            height: 32
            color: selected ? Look.accent : mouse.containsMouse ? Look.raised : Look.surface
            border.color: selected ? Look.accent : Look.line
            border.width: 1
            radius: (first || last) ? Look.radius : 0
            opacity: root.active ? 1 : 0.5
            activeFocusOnTab: root.active

            // square off the inner corners of the end segments
            Rectangle {
                visible: !(seg.first && seg.last) && (seg.first || seg.last)
                width: Look.radius
                height: parent.height
                x: seg.first ? parent.width - width : 0
                color: parent.color
                border.width: 0
                Rectangle { width: parent.width; height: 1; color: seg.border.color; anchors.top: parent.top }
                Rectangle { width: parent.width; height: 1; color: seg.border.color; anchors.bottom: parent.bottom }
            }

            Text {
                id: caption
                anchors.centerIn: parent
                text: seg.label
                color: seg.selected ? Look.onAccent : Look.text
                font.pixelSize: Look.body
                font.weight: seg.selected ? Font.DemiBold : Font.Normal
            }

            Rectangle {
                anchors.fill: parent
                anchors.margins: -2
                radius: Look.radius + 2
                color: "transparent"
                border.color: Look.focus
                border.width: 2
                visible: seg.activeFocus
            }

            MouseArea {
                id: mouse
                anchors.fill: parent
                hoverEnabled: true
                enabled: root.active
                cursorShape: Qt.PointingHandCursor
                onClicked: { seg.forceActiveFocus(); if (!seg.selected) root.picked(seg.val) }
            }
            Keys.onSpacePressed: if (!seg.selected) root.picked(seg.val)
            Keys.onReturnPressed: if (!seg.selected) root.picked(seg.val)
        }
    }
}
