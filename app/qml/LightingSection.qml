import QtQuick
import QtQuick.Controls.Basic
import QtQuick.Layouts

Column {
    id: root
    property var device: ({})
    readonly property var zones: device.lighting ? device.lighting.zones : []
    readonly property string devId: device.id || ""

    property string zone: zones.length > 1 ? "all" : (zones.length ? zones[0].id : "all")
    readonly property var zoneObj: zone === "all" ? (zones[0] || {}) : (zones.find(z => z.id === zone) || {})
    readonly property var effects: {
        if (zone !== "all") return zoneObj.effects || []
        const out = []
        for (const z of zones) for (const e of z.effects) if (out.indexOf(e) < 0) out.push(e)
        return out
    }
    readonly property bool hasBrightness: zone === "all"
        ? zones.some(z => z.brightness !== null && z.brightness !== undefined)
        : (zoneObj.brightness !== null && zoneObj.brightness !== undefined)
    readonly property bool dual: zone === "all" ? zones.some(z => z.dual_color) : !!zoneObj.dual_color

    // what the controls show; loaded from the zone's last known effect
    property string effect: "static"
    property color color: "#9bd13e"
    property color color2: "#1fb8ff"
    property bool useColor2: false
    property int speed: 50
    property string direction: "right"

    function load() {
        const cur = zoneObj ? zoneObj.current : null
        if (cur) {
            effect = cur.effect
            color = cur.color
            useColor2 = !!cur.color2
            if (cur.color2) color2 = cur.color2
            speed = cur.speed
            direction = cur.direction
        } else if (effects && effects.indexOf(effect) < 0) {
            effect = effects.indexOf("static") >= 0 ? "static" : (effects[0] || "off")
        }
    }
    onZoneChanged: load()
    Component.onCompleted: load()

    readonly property bool usesColor: ["static", "breathing", "reactive", "ripple"].indexOf(effect) >= 0
    readonly property bool usesSpeed: ["breathing", "spectrum", "wave", "reactive", "ripple"].indexOf(effect) >= 0

    function send() {
        const v = { zone: zone, effect: effect, color: String(color), speed: speed, direction: direction }
        if (effect === "breathing" && dual && useColor2) v.color2 = String(color2)
        Daemon.setSetting(devId, "lighting", v)
    }

    readonly property var labels: ({
        off: qsTr("Off"), static: qsTr("Solid"), breathing: qsTr("Breathing"), spectrum: qsTr("Color cycle"),
        wave: qsTr("Wave"), reactive: qsTr("React to clicks"), ripple: qsTr("Ripple")
    })

    spacing: 20

    SettingRow {
        visible: root.zones.length > 1
        label: qsTr("Zone")
        Segmented {
            options: [{ value: "all", label: qsTr("All zones") }].concat(root.zones.map(z => ({ value: z.id, label: z.name })))
            current: root.zone
            onPicked: (v) => root.zone = v
        }
    }

    SettingRow {
        label: qsTr("Effect")
        Segmented {
            options: root.effects.map(e => ({ value: e, label: root.labels[e] || Look.sentence(e) }))
            current: root.effect
            onPicked: (v) => { root.effect = v; root.send() }
        }
    }

    Column {
        visible: root.usesColor
        width: parent.width
        spacing: 10
        Text { text: qsTr("Color"); color: Look.text; font.pixelSize: Look.body }
        ColorPicker {
            width: parent.width
            color: root.color
            onPicked: (c) => { root.color = c; root.send() }
        }
    }

    Column {
        visible: root.effect === "breathing" && root.dual
        width: parent.width
        spacing: 10
        SettingRow {
            label: qsTr("Alternate with a second color")
            Switch {
                id: second
                checked: root.useColor2
                onToggled: { root.useColor2 = checked; root.send() }
                indicator: Rectangle {
                    implicitWidth: 40; implicitHeight: 22; radius: 11
                    color: second.checked ? Look.accent : Look.line
                    border.color: second.visualFocus ? Look.focus : "transparent"
                    border.width: 2
                    Rectangle {
                        x: second.checked ? parent.width - width - 3 : 3
                        anchors.verticalCenter: parent.verticalCenter
                        width: 16; height: 16; radius: 8
                        color: Look.surface
                        Behavior on x { NumberAnimation { duration: 120 } }
                    }
                }
            }
        }
        ColorPicker {
            visible: root.useColor2
            width: parent.width
            color: root.color2
            onPicked: (c) => { root.color2 = c; root.send() }
        }
    }

    SettingRow {
        visible: root.usesSpeed
        label: qsTr("Speed")
        PSlider {
            from: 0; to: 100; stepSize: 5
            deviceValue: root.speed
            onCommitted: (v) => { root.speed = v; root.send() }
        }
    }

    SettingRow {
        visible: root.effect === "wave"
        label: qsTr("Direction")
        Segmented {
            options: [{ value: "left", label: qsTr("Leftward") }, { value: "right", label: qsTr("Rightward") }]
            current: root.direction
            onPicked: (v) => { root.direction = v; root.send() }
        }
    }

    SettingRow {
        visible: root.hasBrightness
        label: qsTr("Brightness")
        value: brightness.value.toFixed(0) + "%"
        PSlider {
            id: brightness
            from: 0; to: 100; stepSize: 1
            deviceValue: root.zone === "all"
                ? Math.max.apply(null, root.zones.map(z => z.brightness || 0))
                : (root.zoneObj.brightness || 0)
            onCommitted: (v) => Daemon.setSetting(root.devId, "brightness", { zone: root.zone, value: Math.round(v) })
        }
    }

    Text {
        width: parent.width
        wrapMode: Text.Wrap
        visible: !root.zoneObj.current
        text: qsTr("This device doesn't report its current effect. Pick one and Peridot will keep it applied.")
        color: Look.faint
        font.pixelSize: Look.small
    }
}
