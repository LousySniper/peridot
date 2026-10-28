#include "singleinstance.h"

#include <QDBusConnection>
#include <QDBusConnectionInterface>
#include <QDBusMessage>

static const char *SERVICE = "io.github.peridot.App";
static const char *PATH = "/io/github/peridot/App";
static const char *IFACE = "io.github.peridot.App";

SingleInstance::SingleInstance(QObject *parent)
    : QObject(parent)
{
}

bool SingleInstance::claim(bool minimized, const QString &device, const QString &section)
{
    QDBusConnection bus = QDBusConnection::sessionBus();
    // Without a session bus there is nothing to coordinate with; just run.
    if (!bus.isConnected() || !bus.interface())
        return true;

    bus.registerObject(PATH, this, QDBusConnection::ExportScriptableSlots);
    const auto reply = bus.interface()->registerService(SERVICE, QDBusConnectionInterface::DontQueueService,
                                                        QDBusConnectionInterface::DontAllowReplacement);
    if (reply.isValid() && reply.value() == QDBusConnectionInterface::ServiceRegistered)
        return true;
    bus.unregisterObject(PATH);

    // Another instance owns the name. A tray-only start (login autostart) has
    // nothing to ask of it; anything else should bring its window up.
    if (!minimized) {
        QDBusMessage m = QDBusMessage::createMethodCall(SERVICE, PATH, IFACE, "Activate");
        m.setArguments({device, section});
        const QDBusMessage r = bus.call(m, QDBus::Block, 3000);
        // If the owner didn't answer it is wedged or exiting; run instead of
        // leaving the user with no window at all.
        if (r.type() == QDBusMessage::ErrorMessage)
            return true;
    }
    return false;
}

void SingleInstance::Activate(const QString &device, const QString &section)
{
    emit activateRequested(device, section);
}
