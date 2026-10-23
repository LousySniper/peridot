import QtQuick
import QtQuick.Controls.Basic
import QtQuick.Layouts

Column {
    id: root
    property var device: ({})
    readonly property var controls: device.buttons ? device.buttons.controls : []
    readonly property string devId: device.id || ""
    readonly property string actionProblem: Daemon.status.uinput_error || ""

    spacing: 4

    function labelFor(action) {
        const hit = Daemon.actions.find(a => a.id === action)
        if (hit) return hit.label
        if (action.startsWith("key:")) return action.slice(4).split("+").map(Look.sentence).join(" + ")
        return Look.sentence(action.replace(":", " "))
    }

    SectionTitle {
        text: qsTr("Buttons")
        hint: device.vendor === "razer"
              ? qsTr("Pick what each button does. Remapped buttons are handled by Peridot while it runs.")
              : qsTr("Pick what each button does. Buttons left on Default behave exactly as the mouse intends.")
    }

    Rectangle {
        visible: root.actionProblem.length > 0
        width: parent.width
        implicitHeight: warn.implicitHeight + 20
        radius: Look.radius
        color: "transparent"
        border.color: Look.warn
        Text {
            id: warn
            anchors.fill: parent
            anchors.margins: 10
            wrapMode: Text.Wrap
            color: Look.warn
            font.pixelSize: Look.body
            text: root.actionProblem + " " + qsTr("Mouse buttons and DPI actions work regardless.")
        }
    }

    Item { width: 1; height: 8 }

    Repeater {
        model: root.controls.length
        delegate: RowLayout {
            id: row
            required property int index
            readonly property var modelData: root.controls[index]
            width: root.width
            height: 48
            spacing: 16

            Text {
                text: row.modelData.name
                color: row.modelData.remappable ? Look.text : Look.muted
                font.pixelSize: Look.body
                Layout.fillWidth: true
            }
            Text {
                visible: !row.modelData.remappable
                text: qsTr("Always left click")
                color: Look.faint
                font.pixelSize: Look.small
            }
            ComboBox {
                id: combo
                visible: row.modelData.remappable
                implicitWidth: 280
                implicitHeight: 34
                readonly property var entries: {
                    const list = Daemon.actions.map(a => ({ id: a.id, label: a.label, group: a.group }))
                    const cur = row.modelData.action
                    if (!list.some(a => a.id === cur))
                        list.splice(2, 0, { id: cur, label: root.labelFor(cur), group: qsTr("Custom") })
                    list.push({ id: "__custom", label: qsTr("Custom shortcut…"), group: qsTr("Custom") })
                    return list
                }
                model: entries
                textRole: "label"
                valueRole: "id"
                currentIndex: entries.findIndex(e => e.id === row.modelData.action)
                Accessible.name: qsTr("Action for %1").arg(row.modelData.name)
                onActivated: (i) => {
                    const id = entries[i].id
                    if (id === "__custom") {
                        shortcut.controlName = row.modelData.name
                        shortcut.controlId = row.modelData.id
                        shortcut.open()
                        currentIndex = entries.findIndex(e => e.id === row.modelData.action)
                        return
                    }
                    Daemon.setSetting(root.devId, "button", { control: row.modelData.id, action: id })
                }

                background: Rectangle {
                    radius: Look.radius
                    color: combo.hovered ? Look.raised : Look.surface
                    border.color: combo.visualFocus ? Look.focus : Look.line
                    border.width: combo.visualFocus ? 2 : 1
                }
                contentItem: Text {
                    leftPadding: 12
                    rightPadding: 28
                    text: combo.displayText
                    color: row.modelData.action === "default" ? Look.muted : Look.text
                    font.pixelSize: Look.body
                    verticalAlignment: Text.AlignVCenter
                    elide: Text.ElideRight
                }
                indicator: Text {
                    x: combo.width - width - 12
                    anchors.verticalCenter: parent.verticalCenter
                    text: "⌄"
                    color: Look.muted
                    font.pixelSize: Look.sub
                }
                delegate: ItemDelegate {
                    id: item
                    required property var modelData
                    required property int index
                    width: combo.width
                    highlighted: combo.highlightedIndex === index
                    contentItem: RowLayout {
                        Text {
                            text: item.modelData.label
                            color: Look.text
                            font.pixelSize: Look.body
                            Layout.fillWidth: true
                            elide: Text.ElideRight
                        }
                        Text {
                            text: item.modelData.group
                            color: Look.faint
                            font.pixelSize: Look.small
                        }
                    }
                    background: Rectangle { color: item.highlighted ? Look.accentSoft : "transparent" }
                }
                popup.background: Rectangle { radius: Look.radius; color: Look.surface; border.color: Look.line }
            }
        }
    }

    ShortcutDialog {
        id: shortcut
        property string controlId: ""
        onAccepted: (action) => Daemon.setSetting(root.devId, "button", { control: controlId, action: action })
    }
}
