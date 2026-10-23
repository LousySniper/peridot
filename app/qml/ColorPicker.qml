import QtQuick
import QtQuick.Layouts

// Preset swatches, a hue strip for anything else, and a hex field.
ColumnLayout {
    id: root
    property color color: "#9bd13e"
    signal picked(color color)

    spacing: 10

    readonly property var presets: ["#ffffff", "#ff2d2d", "#ff8a1f", "#ffd21f", "#9bd13e", "#1fd18a", "#1fb8ff", "#3d5bff", "#9a3dff", "#ff3dbd"]

    function hex(c) {
        const h = (x) => ("0" + Math.round(x * 255).toString(16)).slice(-2)
        return "#" + h(c.r) + h(c.g) + h(c.b)
    }

    Flow {
        Layout.fillWidth: true
        spacing: 8
        Repeater {
            model: root.presets
            delegate: Rectangle {
                required property string modelData
                readonly property bool chosen: root.hex(root.color) === modelData
                width: 28; height: 28; radius: 14
                color: modelData
                border.color: chosen ? Look.text : Look.line
                border.width: chosen ? 3 : 1
                activeFocusOnTab: true
                Accessible.name: modelData
                Rectangle {
                    anchors.fill: parent; anchors.margins: -3; radius: 17
                    color: "transparent"; border.color: Look.focus; border.width: 2
                    visible: parent.activeFocus
                }
                MouseArea {
                    anchors.fill: parent
                    cursorShape: Qt.PointingHandCursor
                    onClicked: root.picked(parent.modelData)
                }
                Keys.onSpacePressed: root.picked(modelData)
            }
        }
    }

    RowLayout {
        Layout.fillWidth: true
        spacing: 12

        // hue strip
        Item {
            id: strip
            Layout.fillWidth: true
            implicitHeight: 20
            Rectangle {
                anchors.fill: parent
                radius: 10
                gradient: Gradient {
                    orientation: Gradient.Horizontal
                    GradientStop { position: 0.0; color: "#ff0000" }
                    GradientStop { position: 0.17; color: "#ffff00" }
                    GradientStop { position: 0.33; color: "#00ff00" }
                    GradientStop { position: 0.5; color: "#00ffff" }
                    GradientStop { position: 0.67; color: "#0000ff" }
                    GradientStop { position: 0.83; color: "#ff00ff" }
                    GradientStop { position: 1.0; color: "#ff0000" }
                }
            }
            Rectangle {
                readonly property real h: root.color.hsvHue < 0 ? 0 : root.color.hsvHue
                x: h * (strip.width - width)
                y: -3
                width: 12; height: 26; radius: 4
                color: "transparent"
                border.color: Look.text
                border.width: 2
            }
            MouseArea {
                anchors.fill: parent
                cursorShape: Qt.PointingHandCursor
                function hueAt(x) { return Math.max(0, Math.min(0.999, x / strip.width)) }
                onPressed: (m) => root.color = Qt.hsva(hueAt(m.x), 1, 1, 1)
                onPositionChanged: (m) => root.color = Qt.hsva(hueAt(m.x), 1, 1, 1)
                onReleased: root.picked(root.color)
            }
        }

        Rectangle {
            implicitWidth: 92
            implicitHeight: 30
            radius: Look.radius
            color: Look.surface
            border.color: hexField.activeFocus ? Look.focus : Look.line
            border.width: hexField.activeFocus ? 2 : 1
            Row {
                anchors.fill: parent
                anchors.leftMargin: 8
                spacing: 6
                Rectangle { width: 14; height: 14; radius: 3; color: root.color; anchors.verticalCenter: parent.verticalCenter; border.color: Look.line }
                TextInput {
                    id: hexField
                    anchors.verticalCenter: parent.verticalCenter
                    width: 60
                    text: root.hex(root.color)
                    color: Look.text
                    font.pixelSize: Look.body
                    selectByMouse: true
                    validator: RegularExpressionValidator { regularExpression: /#?[0-9a-fA-F]{6}/ }
                    Accessible.name: qsTr("Color code")
                    onEditingFinished: {
                        const t = text.startsWith("#") ? text : "#" + text
                        if (acceptableInput) root.picked(t)
                    }
                }
            }
        }
    }
}
