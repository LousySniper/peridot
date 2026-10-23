import QtQuick

// Brief confirmation or error at the bottom of the window.
Rectangle {
    id: root
    property bool error: false

    function show(message, isError) {
        label.text = message
        error = !!isError
        opacity = 1
        hide.interval = isError ? 6000 : 2200
        hide.restart()
    }

    anchors.horizontalCenter: parent.horizontalCenter
    anchors.bottom: parent.bottom
    anchors.bottomMargin: 20
    width: Math.min(label.implicitWidth + 32, parent.width - 48)
    height: label.implicitHeight + 18
    radius: Look.radius
    color: Look.dark ? "#f0f2ee" : "#26303c"
    border.color: error ? Look.danger : "transparent"
    border.width: 2
    opacity: 0
    visible: opacity > 0
    Behavior on opacity { NumberAnimation { duration: 160 } }

    Text {
        id: label
        anchors.centerIn: parent
        width: Math.min(implicitWidth, root.parent.width - 80)
        wrapMode: Text.Wrap
        color: Look.dark ? "#1e2530" : "#f0f2ee"
        font.pixelSize: Look.body
    }

    Timer { id: hide; onTriggered: root.opacity = 0 }
}
