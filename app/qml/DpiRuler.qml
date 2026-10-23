import QtQuick

// The sensor's whole range on one logarithmic track. Each DPI stage is a
// marker you can drag; the stage in use is lit. Double-click the track to add
// a stage.
Item {
    id: root
    property var dpi: ({})
    property var stages: []          // working copy, edited while dragging
    property int active: 0
    signal stagesEdited(var stages, int active)
    signal activated(int index)

    readonly property real lo: Math.max(50, dpi.min || 100)
    readonly property real hi: Math.max(lo * 2, dpi.max || 16000)
    readonly property real trackY: 58
    readonly property real inset: 14

    implicitHeight: 100

    function xFor(v) {
        const t = (Math.log(Math.max(lo, Math.min(hi, v))) - Math.log(lo)) / (Math.log(hi) - Math.log(lo))
        return inset + t * (width - 2 * inset)
    }
    function valueAt(x) {
        const t = Math.max(0, Math.min(1, (x - inset) / (width - 2 * inset)))
        return snap(Math.exp(Math.log(lo) + t * (Math.log(hi) - Math.log(lo))))
    }
    function snap(v) {
        if (dpi.list && dpi.list.length > 0) {
            let best = dpi.list[0]
            for (const c of dpi.list) if (Math.abs(c - v) < Math.abs(best - v)) best = c
            return best
        }
        const step = dpi.step > 1 ? dpi.step : (v < 1000 ? 10 : 50)
        const min = dpi.min || 100
        return Math.max(min, Math.min(hi, Math.round((v - min) / step) * step + min))
    }
    function fmt(v) {
        return Number(v).toLocaleString(Qt.locale(), "f", 0)
    }
    readonly property var ticks: {
        const out = []
        // Leave room for the end labels, which sit beside the range ends.
        for (let v = 100; v <= hi; v *= 2) if (v > lo * 1.7 && v < hi / 1.7) out.push(v)
        return out
    }

    // track
    Rectangle {
        x: root.inset
        y: root.trackY - 1
        width: root.width - 2 * root.inset
        height: 2
        radius: 1
        color: Look.line
    }
    // span covered by stages
    Rectangle {
        visible: root.stages.length > 1
        x: root.xFor(Math.min.apply(null, root.stages))
        y: root.trackY - 1.5
        width: root.xFor(Math.max.apply(null, root.stages)) - x
        height: 3
        color: Look.faint
        opacity: 0.6
    }

    // range ends and ticks
    Repeater {
        model: root.ticks.length + 2
        delegate: Item {
            required property int index
            readonly property var modelData: index === 0 ? root.lo : index === root.ticks.length + 1 ? root.hi : root.ticks[index - 1]
            readonly property bool end: index === 0 || index === root.ticks.length + 1
            x: root.xFor(modelData)
            y: root.trackY + 8
            Rectangle { x: -0.5; y: 0; width: 1; height: parent.end ? 7 : 4; color: Look.faint }
            Text {
                y: 10
                x: parent.end && parent.index > 0 ? -implicitWidth : (parent.end ? 0 : -implicitWidth / 2)
                text: root.fmt(parent.modelData)
                color: Look.faint
                font.pixelSize: 11
                font.features: { "tnum": 1 }
            }
        }
    }

    MouseArea {
        anchors.fill: parent
        onDoubleClicked: (m) => {
            const max = root.dpi.max_stages || 5
            if (root.stages.length >= max) return
            const v = root.valueAt(m.x)
            if (root.stages.indexOf(v) >= 0) return
            const activeValue = root.stages[root.active]
            const next = root.stages.concat([v]).sort((a, b) => a - b)
            root.stagesEdited(next, next.indexOf(activeValue))
        }
    }

    Repeater {
        model: root.stages.length
        delegate: Item {
            id: marker
            required property int index
            readonly property bool isActive: index === root.active
            property real dragValue: root.stages[index]
            readonly property real shown: dragState.active ? dragValue : root.stages[index]
            x: root.xFor(shown)
            y: root.trackY
            z: isActive || dragState.active ? 2 : 1

            Rectangle {
                id: knob
                width: marker.isActive ? 20 : 14
                height: width
                radius: width / 2
                x: -width / 2
                y: -height / 2
                color: marker.isActive ? Look.accent : Look.window
                border.color: marker.isActive ? Look.accent : Look.text
                border.width: 2
                Behavior on width { NumberAnimation { duration: 120 } }
            }
            Text {
                x: -implicitWidth / 2
                y: -38
                text: root.fmt(marker.shown)
                color: marker.isActive ? Look.accent : Look.text
                font.pixelSize: marker.isActive ? Look.sub : Look.body
                font.weight: marker.isActive ? Font.DemiBold : Font.Normal
                font.features: { "tnum": 1 }
            }
            MouseArea {
                id: area
                x: -16; y: -40; width: 32; height: 56
                cursorShape: dragState.active ? Qt.ClosedHandCursor : Qt.PointingHandCursor
                preventStealing: true
                property real startX: 0
                property bool moved: false
                onPressed: (m) => { startX = m.x; moved = false; marker.dragValue = root.stages[marker.index] }
                onPositionChanged: (m) => {
                    if (!pressed) return
                    if (Math.abs(m.x - startX) > 3) moved = true
                    if (moved) marker.dragValue = root.valueAt(area.mapToItem(root, m.x, 0).x)
                }
                onReleased: {
                    if (moved) {
                        const next = root.stages.slice()
                        next[marker.index] = marker.dragValue
                        const activeValue = marker.isActive ? marker.dragValue : root.stages[root.active]
                        next.sort((a, b) => a - b)
                        root.stagesEdited(next, next.indexOf(activeValue))
                    } else if (!marker.isActive) {
                        root.activated(marker.index)
                    }
                    moved = false
                }
            }
            QtObject { id: dragState; readonly property bool active: area.pressed && area.moved }
        }
    }
}
