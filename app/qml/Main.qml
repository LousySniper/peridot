import QtQuick
import QtQuick.Controls.Basic
import QtQuick.Layouts

ApplicationWindow {
    id: win
    width: 1000
    height: 680
    minimumWidth: 760
    minimumHeight: 520
    title: qsTr("Peridot")
    color: Look.window

    property string currentId: ""
    property string section: InitialSection
    property string pendingSelect: InitialDevice
    // re-evaluated whenever the device list changes
    readonly property var current: { Daemon.devices; return currentId ? Daemon.device(currentId) : ({}) }
    readonly property var warnings: (Daemon.status.warnings || []).filter(w => w.indexOf("uinput") < 0)

    // keep a valid selection as devices come and go
    function reconcile() {
        const list = Daemon.devices
        if (list.length === 0) { currentId = ""; return }
        if (!list.some(d => d.id === currentId)) currentId = list[0].id
    }
    function selectDevice(text) {
        const t = String(text).toLowerCase()
        const hit = Daemon.devices.find(d => d.id === text || d.name.toLowerCase().indexOf(t) >= 0)
        if (hit) currentId = hit.id
    }
    Connections {
        target: Daemon
        function onDevicesChanged() {
            if (win.pendingSelect && Daemon.devices.length > 0) {
                win.selectDevice(win.pendingSelect)
                win.pendingSelect = ""
            }
            win.reconcile()
        }
        function onSettingSaved(id, key, deferred) {
            if (deferred) toast.show(qsTr("Saved. It will apply when the device wakes up."), false)
        }
        function onSettingFailed(id, key, message) { toast.show(message, true) }
        function onForgotten(id) { toast.show(qsTr("Saved settings removed"), false) }
        function onDaemonStartFailed(message) { toast.show(qsTr("Couldn't start the Peridot service: %1").arg(message), true) }
    }
    Component.onCompleted: {
        reconcile()
    }

    onClosing: (close) => {
        if (TrayAvailable) {
            close.accepted = false
            win.hide()
        }
    }

    RowLayout {
        anchors.fill: parent
        spacing: 0
        visible: Daemon.available && Daemon.devices.length > 0

        DeviceList {
            Layout.preferredWidth: 248
            Layout.fillHeight: true
            currentId: win.currentId
            onChosen: (id) => win.currentId = id
        }
        Rectangle { Layout.fillHeight: true; width: 1; color: Look.line }
        DevicePage {
            Layout.fillWidth: true
            Layout.fillHeight: true
            device: win.current
            section: win.section
            onSectionChanged: win.section = section
            onForgetRequested: forget.open()
        }
    }

    EmptyState {
        anchors.fill: parent
        visible: !Daemon.available
        kind: "mouse"
        title: qsTr("The Peridot service isn't running")
        body: qsTr("It talks to your mice and headsets in the background and keeps your settings applied. Start it once and it will run at every login.")
        actionText: qsTr("Start service")
        onAction: Daemon.startDaemon()
    }

    EmptyState {
        anchors.fill: parent
        visible: Daemon.available && Daemon.devices.length === 0
        kind: "headset"
        title: qsTr("No mice or headsets yet")
        body: win.warnings.length > 0
              ? win.warnings.join("\n\n")
              : qsTr("Plug in a Logitech or Razer device, or its wireless receiver. Wireless devices appear as soon as they wake up.")
    }

    Text {
        visible: Daemon.available && Daemon.devices.length > 0 && win.warnings.length > 0
        anchors.left: parent.left
        anchors.bottom: parent.bottom
        anchors.margins: 16
        width: 216
        wrapMode: Text.Wrap
        text: win.warnings[0] || ""
        color: Look.warn
        font.pixelSize: Look.small
    }

    Popup {
        id: forget
        modal: true
        anchors.centerIn: Overlay.overlay
        width: 400
        padding: 24
        background: Rectangle { radius: 10; color: Look.surface; border.color: Look.line }
        Overlay.modal: Rectangle { color: Qt.rgba(0, 0, 0, 0.45) }
        ColumnLayout {
            width: parent.width
            spacing: 14
            Text {
                text: qsTr("Forget settings for %1?").arg(win.current.name || "")
                color: Look.text
                font.pixelSize: Look.sub
                font.weight: Font.DemiBold
                Layout.fillWidth: true
                wrapMode: Text.Wrap
            }
            Text {
                Layout.fillWidth: true
                wrapMode: Text.Wrap
                text: qsTr("Peridot stops re-applying your DPI, lighting, button and sound settings. The device keeps its current state until it reconnects.")
                color: Look.muted
                font.pixelSize: Look.body
            }
            RowLayout {
                Layout.fillWidth: true
                Item { Layout.fillWidth: true }
                PButton { text: qsTr("Keep settings"); quiet: true; onClicked: forget.close() }
                PButton {
                    text: qsTr("Forget settings")
                    destructive: true
                    onClicked: { Daemon.forget(win.currentId); forget.close() }
                }
            }
        }
    }

    Toast { id: toast }
}
