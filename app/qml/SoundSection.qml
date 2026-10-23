import QtQuick
import QtQuick.Controls.Basic
import QtQuick.Layouts

Column {
    id: root
    property var device: ({})
    readonly property var audio: device.audio || ({})
    readonly property var eq: audio.equalizer
    readonly property string devId: device.id || ""

    spacing: 20

    function freqLabel(f) {
        return f >= 1000 ? (f / 1000).toLocaleString(Qt.locale(), "f", f % 1000 ? 1 : 0) + " kHz" : f + " Hz"
    }
    readonly property var pc: audio.pc
    // Presets the headset ships with (from the daemon), else generic ones.
    readonly property var presets: (eq && eq.presets && eq.presets.length) ? eq.presets : [
        { id: "default", name: qsTr("Default") },
        { id: "game", name: qsTr("Game") },
        { id: "music", name: qsTr("Music") },
        { id: "movie", name: qsTr("Movie") }
    ]
    function preset(name) {
        const own = root.presets.find(p => p.id === name && p.gains)
        if (own) return own.gains.map(g => Math.max(eq.min, Math.min(eq.max, g)))
        return eq.bands.map(b => {
            const f = b.freq
            let g = 0
            if (name === "game") g = f < 150 ? -3 : (f >= 2000 && f <= 8000) ? 4 : 0
            else if (name === "music") g = f <= 150 ? 4 : f >= 6000 ? 3 : (f >= 500 && f <= 1000) ? -1 : 0
            else if (name === "movie") g = f <= 150 ? 5 : f >= 4000 ? 3 : 0
            return Math.max(eq.min, Math.min(eq.max, g))
        })
    }
    // Headsets with presets of their own (Razer's "Equalizer preset") don't get the app's too.
    readonly property bool builtInPresets: (audio.modes || []).some(m => m.id === "eq_preset")
    // The curve the user tuned by hand; offered as "Custom" next to the presets.
    readonly property var custom: (eq && eq.custom && eq.custom.length === eq.bands.length) ? eq.custom : null
    readonly property string activePreset: {
        if (!eq) return ""
        const cur = eq.bands.map(b => b.gain)
        for (const p of root.presets)
            if (JSON.stringify(preset(p.id)) === JSON.stringify(cur)) return p.id
        return "custom"
    }

    SettingRow {
        visible: !!root.audio.sidetone
        label: qsTr("Sidetone")
        hint: qsTr("Hear your own voice in the headset while you talk.")
        value: {
            const max = root.audio.sidetone ? root.audio.sidetone.max : 100
            if (sidetone.value === 0) return qsTr("Off")
            return max === 100 ? sidetone.value.toFixed(0) + "%" : qsTr("%1 of %2").arg(sidetone.value.toFixed(0)).arg(max)
        }
        PSlider {
            id: sidetone
            from: 0; to: root.audio.sidetone ? root.audio.sidetone.max : 100; stepSize: 1
            deviceValue: root.audio.sidetone ? root.audio.sidetone.value : 0
            onCommitted: (v) => Daemon.setSetting(root.devId, "sidetone", Math.round(v))
        }
    }

    Repeater {
        model: root.audio.modes || []
        delegate: SettingRow {
            required property var modelData
            label: modelData.name
            hint: modelData.hint
            Segmented {
                options: modelData.options
                current: modelData.value
                onPicked: (v) => Daemon.setSetting(root.devId, "mode", { id: modelData.id, value: v })
            }
        }
    }

    Column {
        visible: !!root.eq
        width: parent.width
        spacing: 14

        ColumnLayout {
            width: parent.width
            spacing: 10
            ColumnLayout {
                Layout.fillWidth: true
                spacing: 2
                Text { text: qsTr("Equalizer"); color: Look.text; font.pixelSize: Look.body }
                Text {
                    visible: !!root.eq && !!root.eq.software
                    Layout.fillWidth: true
                    Layout.rightMargin: 16
                    text: qsTr("Applied on this computer, like G HUB does, to everything playing on the headset. Keep the headset as your output; its volume works as usual.")
                    color: Look.muted
                    font.pixelSize: Look.small
                    wrapMode: Text.Wrap
                }
            }
            Segmented {
                visible: !root.builtInPresets
                options: root.presets.map(p => ({ value: p.id, label: p.name }))
                    .concat(root.custom || root.activePreset === "custom" ? [{ value: "custom", label: qsTr("Custom") }] : [])
                current: root.activePreset
                onPicked: (v) => {
                    if (v !== "custom")
                        Daemon.setSetting(root.devId, "equalizer", root.preset(v))
                    else if (root.custom)
                        Daemon.setSetting(root.devId, "equalizer", { gains: root.custom, custom: true })
                }
            }
        }

        Row {
            spacing: 0
            width: parent.width
            Repeater {
                model: root.eq ? root.eq.bands : []
                delegate: Column {
                    id: band
                    required property var modelData
                    required property int index
                    width: parent.width / root.eq.bands.length
                    spacing: 6
                    Text {
                        anchors.horizontalCenter: parent.horizontalCenter
                        text: (slider.value > 0 ? "+" : "") + slider.value.toFixed(0) + " dB"
                        color: slider.value === 0 ? Look.muted : Look.text
                        font.pixelSize: Look.small
                        font.features: { "tnum": 1 }
                    }
                    Slider {
                        id: slider
                        anchors.horizontalCenter: parent.horizontalCenter
                        orientation: Qt.Vertical
                        from: root.eq.min; to: root.eq.max; stepSize: 1
                        implicitHeight: 150
                        implicitWidth: 32
                        Accessible.name: root.freqLabel(band.modelData.freq)
                        Binding on value {
                            when: !slider.pressed && !hold.running
                            value: band.modelData.gain
                            restoreMode: Binding.RestoreNone
                        }
                        // Keep showing the chosen gain until the device confirms it.
                        Timer { id: hold; interval: 2000 }
                        Connections {
                            target: band
                            function onModelDataChanged() { hold.stop() }
                        }
                        function commit() {
                            hold.restart()
                            const gains = root.eq.bands.map(b => b.gain)
                            gains[band.index] = Math.round(slider.value)
                            Daemon.setSetting(root.devId, "equalizer", { gains: gains, custom: true })
                        }
                        onPressedChanged: if (!pressed) commit()
                        onMoved: if (!pressed) commit()
                        background: Item {
                            x: slider.leftPadding + slider.availableWidth / 2 - 2
                            y: slider.topPadding
                            width: 4
                            height: slider.availableHeight
                            Rectangle { anchors.fill: parent; radius: 2; color: Look.line }
                            // zero line
                            Rectangle {
                                x: -6; width: 16; height: 1; color: Look.faint
                                y: parent.height * (1 - (0 - slider.from) / (slider.to - slider.from))
                            }
                            Rectangle {
                                readonly property real zeroY: parent.height * (1 - (0 - slider.from) / (slider.to - slider.from))
                                readonly property real valY: slider.visualPosition * parent.height
                                width: 4; radius: 2
                                y: Math.min(zeroY, valY)
                                height: Math.abs(zeroY - valY)
                                color: Look.accent
                            }
                        }
                        handle: Rectangle {
                            x: slider.leftPadding + slider.availableWidth / 2 - width / 2
                            y: slider.topPadding + slider.visualPosition * (slider.availableHeight - height)
                            width: 18; height: 18; radius: 9
                            color: Look.surface
                            border.color: slider.activeFocus ? Look.focus : Look.line
                            border.width: slider.activeFocus ? 3 : 2
                        }
                    }
                    Text {
                        anchors.horizontalCenter: parent.horizontalCenter
                        text: root.freqLabel(band.modelData.freq)
                        color: Look.muted
                        font.pixelSize: Look.small
                    }
                }
            }
        }
    }

    SettingRow {
        visible: !!root.audio.auto_off
        label: qsTr("Turn off when idle")
        hint: qsTr("Switches the headset off after this long without sound.")
        Segmented {
            options: root.audio.auto_off
                     ? root.audio.auto_off.options.filter(v => root.audio.auto_off.options.length <= 6
                             || [0, 5, 10, 20, 30, 60].indexOf(v) >= 0 || v === root.audio.auto_off.value)
                         .map(v => ({ value: v, label: v === 0 ? qsTr("Never") : qsTr("%1 min").arg(v) }))
                     : []
            current: root.audio.auto_off ? root.audio.auto_off.value : 0
            onPicked: (v) => Daemon.setSetting(root.devId, "auto_off", v)
        }
    }

    Column {
        visible: !!root.pc
        width: parent.width
        spacing: 20
        topPadding: 8

        SectionTitle {
            width: parent.width
            text: qsTr("On this computer")
        }

        Text {
            visible: !!(root.pc && root.pc.error)
            width: parent.width
            text: root.pc && root.pc.error ? qsTr("Not working right now: %1").arg(root.pc.error) : ""
            color: Look.warn
            font.pixelSize: Look.small
            wrapMode: Text.Wrap
        }

        SettingRow {
            label: qsTr("Virtual 7.1 surround")
            hint: qsTr("Turns surround sound from games and movies into 3D sound on the headset, like DTS Headphone:X in G HUB. Stereo music sounds like speakers in front of you. Switching it briefly interrupts sound.")
            PSwitch {
                checked: !!(root.pc && root.pc.surround)
                onToggled: Daemon.setSetting(root.devId, "surround", checked)
            }
        }
    }
}
