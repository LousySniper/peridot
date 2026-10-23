import QtQuick
import QtQuick.Controls.Basic
import QtQuick.Layouts

Item {
    id: root
    property var device: ({})
    property string section: ""
    signal forgetRequested()

    readonly property var sections: {
        const d = device
        const out = []
        if (d.dpi || d.polling_rate) out.push({ id: "sensitivity", label: qsTr("Sensitivity") })
        if (d.audio) out.push({ id: "sound", label: qsTr("Sound") })
        if (d.lighting) out.push({ id: "lighting", label: qsTr("Lighting") })
        if (d.buttons) out.push({ id: "buttons", label: qsTr("Buttons") })
        if (d.power || d.onboard_mode) out.push({ id: "power", label: qsTr("Power") })
        out.push({ id: "about", label: qsTr("About") })
        return out
    }
    readonly property string shown: sections.some(s => s.id === section) ? section : sections[0].id

    function linkText(d) {
        const parts = []
        if (d.connection === "receiver") parts.push(d.via ? qsTr("Wireless via %1").arg(d.via) : qsTr("Wireless"))
        else if (d.connection === "bluetooth") parts.push(qsTr("Bluetooth"))
        else if (d.connection === "simulated") parts.push(qsTr("Simulated"))
        else parts.push(qsTr("USB"))
        if (!d.online) parts.push(qsTr("asleep"))
        return parts.join(", ")
    }

    ColumnLayout {
        anchors.fill: parent
        spacing: 0

        // header
        RowLayout {
            Layout.fillWidth: true
            Layout.leftMargin: Look.pad + 8
            Layout.rightMargin: Look.pad + 8
            Layout.topMargin: 28
            spacing: 16
            Column {
                Layout.fillWidth: true
                spacing: 4
                Text {
                    width: parent.width
                    text: root.device.name || ""
                    color: Look.text
                    font.pixelSize: Look.title
                    font.weight: Font.DemiBold
                    elide: Text.ElideRight
                }
                Text {
                    text: root.linkText(root.device)
                    color: root.device.online ? Look.muted : Look.warn
                    font.pixelSize: Look.body
                }
            }
            BatteryBadge {
                large: true
                battery: root.device.battery || null
                online: !!root.device.online
                Layout.alignment: Qt.AlignTop
            }
        }

        // section tabs
        Row {
            Layout.leftMargin: Look.pad + 8
            Layout.topMargin: 22
            spacing: 26
            Repeater {
                model: root.sections.length
                delegate: Item {
                    id: tab
                    required property int index
                    readonly property var modelData: root.sections[index]
                    readonly property bool current: root.shown === modelData.id
                    width: label.implicitWidth
                    height: 34
                    activeFocusOnTab: true
                    Accessible.role: Accessible.PageTab
                    Accessible.name: modelData.label
                    Text {
                        id: label
                        text: tab.modelData.label
                        color: tab.current ? Look.text : (tabMouse.containsMouse ? Look.text : Look.muted)
                        font.pixelSize: Look.body
                        font.weight: tab.current ? Font.DemiBold : Font.Normal
                    }
                    Rectangle {
                        anchors.bottom: parent.bottom
                        width: parent.width
                        height: 2
                        radius: 1
                        color: tab.current ? Look.accent : (tab.activeFocus ? Look.focus : "transparent")
                    }
                    MouseArea {
                        id: tabMouse
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: root.section = tab.modelData.id
                    }
                    Keys.onSpacePressed: root.section = modelData.id
                    Keys.onReturnPressed: root.section = modelData.id
                }
            }
        }
        Rectangle { Layout.fillWidth: true; height: 1; color: Look.line }

        // offline banner
        Rectangle {
            visible: !root.device.online
            Layout.fillWidth: true
            implicitHeight: asleep.implicitHeight + 20
            color: Qt.rgba(0.89, 0.69, 0.31, Look.dark ? 0.10 : 0.14)
            Rectangle { width: 3; height: parent.height; color: Look.warn }
            Text {
                id: asleep
                anchors.fill: parent
                anchors.margins: 10
                anchors.leftMargin: Look.pad + 8
                wrapMode: Text.Wrap
                text: (root.device.notes && root.device.notes.length > 0 && root.device.kind === "headset")
                      ? root.device.notes[0]
                      : root.device.kind === "headset"
                        ? qsTr("This headset is off or out of range. Turn it on; changes you make now are saved and applied when it connects.")
                        : qsTr("This device is asleep. Move or click it to wake it; changes you make now are saved and applied when it wakes.")
                color: Look.text
                font.pixelSize: Look.body
            }
        }

        ScrollView {
            id: scroll
            Layout.fillWidth: true
            Layout.fillHeight: true
            contentWidth: availableWidth
            clip: true

            Item {
                width: scroll.availableWidth
                implicitHeight: content.implicitHeight + 56

                Loader {
                    id: content
                    x: Look.pad + 8
                    y: 24
                    width: Math.min(parent.width - 2 * (Look.pad + 8), 760)
                    sourceComponent: {
                        switch (root.shown) {
                        case "sensitivity": return sensitivity
                        case "lighting": return lighting
                        case "buttons": return buttons
                        case "sound": return sound
                        case "power": return power
                        default: return about
                        }
                    }
                }
            }
        }
    }

    Component { id: sensitivity; SensitivitySection { device: root.device } }
    Component { id: lighting; LightingSection { device: root.device } }
    Component { id: buttons; ButtonsSection { device: root.device } }
    Component { id: sound; SoundSection { device: root.device } }
    Component { id: power; PowerSection { device: root.device } }
    Component { id: about; AboutSection { device: root.device; onForgetRequested: root.forgetRequested() } }
}
