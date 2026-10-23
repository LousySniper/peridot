#include "daemonclient.h"

#include <QDBusConnection>
#include <QDBusConnectionInterface>
#include <QDBusMessage>
#include <QDBusPendingCallWatcher>
#include <QDBusPendingReply>
#include <QJSValue>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QProcess>

static const char *SERVICE = "io.github.peridot.Peridot1";
static const char *PATH = "/io/github/peridot/Peridot1";
static const char *IFACE = "io.github.peridot.Manager1";

static QVariant parseJson(const QString &text)
{
    // Wrap so scalars parse too.
    const QJsonDocument doc = QJsonDocument::fromJson(("[" + text + "]").toUtf8());
    if (!doc.isArray() || doc.array().isEmpty())
        return {};
    return doc.array().first().toVariant();
}

static QString toJson(const QVariant &value)
{
    const QJsonValue v = QJsonValue::fromVariant(value);
    QJsonArray wrapper{v};
    QString text = QString::fromUtf8(QJsonDocument(wrapper).toJson(QJsonDocument::Compact));
    return text.mid(1, text.size() - 2);
}

static QDBusMessage call(const QString &method, const QVariantList &args = {})
{
    QDBusMessage m = QDBusMessage::createMethodCall(SERVICE, PATH, IFACE, method);
    m.setArguments(args);
    return m;
}

DaemonClient::DaemonClient(QObject *parent)
    : QObject(parent)
{
    auto bus = QDBusConnection::sessionBus();
    m_watcher = new QDBusServiceWatcher(SERVICE, bus,
                                        QDBusServiceWatcher::WatchForRegistration |
                                            QDBusServiceWatcher::WatchForUnregistration,
                                        this);
    connect(m_watcher, &QDBusServiceWatcher::serviceRegistered, this, &DaemonClient::onServiceRegistered);
    connect(m_watcher, &QDBusServiceWatcher::serviceUnregistered, this, &DaemonClient::onServiceUnregistered);
    bus.connect(SERVICE, PATH, IFACE, "DeviceAdded", this, SLOT(onDeviceAdded(QString)));
    bus.connect(SERVICE, PATH, IFACE, "DeviceChanged", this, SLOT(onDeviceChanged(QString)));
    bus.connect(SERVICE, PATH, IFACE, "DeviceRemoved", this, SLOT(onDeviceRemoved(QString)));
    reload();
}

QVariantList DaemonClient::devices() const
{
    QVariantList out;
    for (const auto &id : m_order)
        out << m_devices.value(id);
    return out;
}

QVariantMap DaemonClient::device(const QString &id) const
{
    return m_devices.value(id);
}

void DaemonClient::setAvailable(bool a)
{
    if (a == m_available)
        return;
    m_available = a;
    emit availableChanged();
}

void DaemonClient::reload()
{
    auto bus = QDBusConnection::sessionBus();
    auto *w = new QDBusPendingCallWatcher(bus.asyncCall(call("ListDevices")), this);
    connect(w, &QDBusPendingCallWatcher::finished, this, [this](QDBusPendingCallWatcher *w) {
        w->deleteLater();
        QDBusPendingReply<QString> r = *w;
        if (r.isError()) {
            setAvailable(false);
            m_order.clear();
            m_devices.clear();
            emit devicesChanged();
            return;
        }
        setAvailable(true);
        m_order.clear();
        m_devices.clear();
        for (const QVariant &d : parseJson(r.value()).toList()) {
            const QVariantMap map = d.toMap();
            const QString id = map.value("id").toString();
            m_order << id;
            m_devices.insert(id, map);
        }
        emit devicesChanged();
    });
    auto *wa = new QDBusPendingCallWatcher(bus.asyncCall(call("GetActions")), this);
    connect(wa, &QDBusPendingCallWatcher::finished, this, [this](QDBusPendingCallWatcher *w) {
        w->deleteLater();
        QDBusPendingReply<QString> r = *w;
        if (!r.isError()) {
            m_actions = parseJson(r.value()).toList();
            emit actionsChanged();
        }
    });
    fetchStatus();
}

void DaemonClient::fetchStatus()
{
    auto *w = new QDBusPendingCallWatcher(QDBusConnection::sessionBus().asyncCall(call("GetStatus")), this);
    connect(w, &QDBusPendingCallWatcher::finished, this, [this](QDBusPendingCallWatcher *w) {
        w->deleteLater();
        QDBusPendingReply<QString> r = *w;
        if (!r.isError()) {
            m_status = parseJson(r.value()).toMap();
            emit statusChanged();
        }
    });
}

void DaemonClient::upsert(const QVariantMap &dev, bool notify)
{
    const QString id = dev.value("id").toString();
    if (id.isEmpty())
        return;
    if (!m_devices.contains(id))
        m_order << id;
    m_devices.insert(id, dev);
    if (notify) {
        emit devicesChanged();
        emit deviceUpdated(id);
    }
}

void DaemonClient::onDeviceAdded(const QString &json)
{
    setAvailable(true);
    upsert(parseJson(json).toMap(), true);
    fetchStatus();
}

void DaemonClient::onDeviceChanged(const QString &json)
{
    upsert(parseJson(json).toMap(), true);
}

void DaemonClient::onDeviceRemoved(const QString &id)
{
    m_order.removeAll(id);
    m_devices.remove(id);
    emit devicesChanged();
    fetchStatus();
}

void DaemonClient::onServiceRegistered()
{
    reload();
}

void DaemonClient::onServiceUnregistered()
{
    setAvailable(false);
    m_order.clear();
    m_devices.clear();
    emit devicesChanged();
}

void DaemonClient::setSetting(const QString &id, const QString &key, const QVariant &value)
{
    QVariant v = value;
    if (v.userType() == qMetaTypeId<QJSValue>())
        v = v.value<QJSValue>().toVariant();
    auto *w = new QDBusPendingCallWatcher(
        QDBusConnection::sessionBus().asyncCall(call("SetSetting", {id, key, toJson(v)}), 30000), this);
    connect(w, &QDBusPendingCallWatcher::finished, this, [this, id, key](QDBusPendingCallWatcher *w) {
        w->deleteLater();
        QDBusPendingReply<QString> r = *w;
        if (r.isError()) {
            if (r.error().name() == QLatin1String("io.github.peridot.Error.Offline")) {
                emit settingSaved(id, key, true);
                return;
            }
            QString msg = r.error().message();
            if (!msg.isEmpty())
                msg[0] = msg[0].toUpper();
            emit settingFailed(id, key, msg);
            return;
        }
        upsert(parseJson(r.value()).toMap(), true);
        emit settingSaved(id, key, false);
    });
}

void DaemonClient::refresh(const QString &id)
{
    auto *w = new QDBusPendingCallWatcher(QDBusConnection::sessionBus().asyncCall(call("Refresh", {id})), this);
    connect(w, &QDBusPendingCallWatcher::finished, this, [this](QDBusPendingCallWatcher *w) {
        w->deleteLater();
        QDBusPendingReply<QString> r = *w;
        if (!r.isError())
            upsert(parseJson(r.value()).toMap(), true);
    });
}

void DaemonClient::forget(const QString &id)
{
    auto *w = new QDBusPendingCallWatcher(QDBusConnection::sessionBus().asyncCall(call("ForgetDevice", {id})), this);
    connect(w, &QDBusPendingCallWatcher::finished, this, [this, id](QDBusPendingCallWatcher *w) {
        w->deleteLater();
        if (!w->isError())
            emit forgotten(id);
    });
}

void DaemonClient::startDaemon()
{
    auto *p = new QProcess(this);
    connect(p, &QProcess::finished, this, [this, p](int code, QProcess::ExitStatus) {
        p->deleteLater();
        if (code != 0) {
            const QString err = QString::fromUtf8(p->readAllStandardError()).trimmed();
            emit daemonStartFailed(err.isEmpty() ? tr("systemctl exited with code %1").arg(code) : err);
        }
    });
    connect(p, &QProcess::errorOccurred, this, [this](QProcess::ProcessError) {
        emit daemonStartFailed(tr("Could not run systemctl."));
    });
    p->start("systemctl", {"--user", "enable", "--now", "peridotd.service"});
}
