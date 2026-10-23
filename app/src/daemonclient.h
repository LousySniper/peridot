#pragma once

#include <QDBusServiceWatcher>
#include <QHash>
#include <QObject>
#include <QVariant>

// Talks to peridotd over the session bus. Every payload is JSON; this class
// turns it into QVariantMaps that QML binds to directly.
class DaemonClient : public QObject {
    Q_OBJECT
    Q_PROPERTY(QVariantList devices READ devices NOTIFY devicesChanged)
    Q_PROPERTY(bool available READ available NOTIFY availableChanged)
    Q_PROPERTY(QVariantMap status READ status NOTIFY statusChanged)
    Q_PROPERTY(QVariantList actions READ actions NOTIFY actionsChanged)

public:
    explicit DaemonClient(QObject *parent = nullptr);

    QVariantList devices() const;
    bool available() const { return m_available; }
    QVariantMap status() const { return m_status; }
    QVariantList actions() const { return m_actions; }

    Q_INVOKABLE QVariantMap device(const QString &id) const;
    // value may be a number, bool, string, list or map; it's sent as JSON.
    Q_INVOKABLE void setSetting(const QString &id, const QString &key, const QVariant &value);
    Q_INVOKABLE void refresh(const QString &id);
    Q_INVOKABLE void forget(const QString &id);
    Q_INVOKABLE void startDaemon();
    Q_INVOKABLE void reload();

signals:
    void devicesChanged();
    void deviceUpdated(const QString &id);
    void availableChanged();
    void statusChanged();
    void actionsChanged();
    void settingSaved(const QString &id, const QString &key, bool deferred);
    void settingFailed(const QString &id, const QString &key, const QString &message);
    void forgotten(const QString &id);
    void daemonStartFailed(const QString &message);

private slots:
    void onDeviceAdded(const QString &json);
    void onDeviceChanged(const QString &json);
    void onDeviceRemoved(const QString &id);
    void onServiceRegistered();
    void onServiceUnregistered();

private:
    void upsert(const QVariantMap &dev, bool notify);
    void setAvailable(bool a);
    void fetchStatus();

    QStringList m_order;
    QHash<QString, QVariantMap> m_devices;
    bool m_available = false;
    QVariantMap m_status;
    QVariantList m_actions;
    QDBusServiceWatcher *m_watcher;
};
