import QtQuick

// Battery as a figure plus a slim bar. `large` is used in the device header.
Item {
    id: root
    property var battery: null
    property bool online: true
    property bool large: false

    readonly property bool known: battery !== null && battery !== undefined && battery.percent !== null && battery.percent !== undefined
    readonly property int pct: known ? battery.percent : 0
    readonly property bool charging: known && battery.charging
    readonly property color tone: Look.batteryColor(known ? pct : null, charging)

    implicitWidth: large ? 132 : 56
    implicitHeight: large ? 52 : 18
    visible: known && online

    Text {
        id: figure
        anchors.right: parent.right
        anchors.top: parent.top
        text: (root.battery && root.battery.estimated ? "≈" : "") + root.pct + "%"
        color: root.tone
        font.pixelSize: root.large ? Look.display : Look.small
        font.weight: root.large ? Font.Light : Font.DemiBold
        font.features: { "tnum": 1 }
    }

    Rectangle {
        visible: root.large
        anchors.right: parent.right
        anchors.top: figure.bottom
        anchors.topMargin: 2
        width: 84
        height: 4
        radius: 2
        color: Look.line
        Rectangle {
            width: parent.width * Math.max(0.04, root.pct / 100)
            height: parent.height
            radius: 2
            color: root.tone
        }
    }

    Text {
        visible: root.large && (root.charging || (root.battery && root.battery.full))
        anchors.right: parent.right
        anchors.bottom: parent.bottom
        anchors.bottomMargin: -16
        text: root.charging ? qsTr("Charging") : qsTr("Fully charged")
        color: Look.accent
        font.pixelSize: Look.small
    }
}
