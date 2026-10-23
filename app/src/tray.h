#pragma once

#include <QHash>
#include <QObject>
#include <QSystemTrayIcon>

class DaemonClient;
class QMenu;

// Tray icon: a battery ring for the device that needs attention first, a menu
// listing every device, and desktop notifications when a battery runs low.
class Tray : public QObject {
    Q_OBJECT
public:
    Tray(DaemonClient *client, QObject *parent = nullptr);
    bool isAvailable() const;

signals:
    void showRequested(const QString &deviceId);
    void quitRequested();

private slots:
    void update();

private:
    QIcon renderIcon(int percent, bool charging, bool available) const;
    void notify(const QString &title, const QString &body, const QString &icon);
    void checkBatteries(const QVariantList &devices);

    DaemonClient *m_client;
    QSystemTrayIcon *m_tray;
    QMenu *m_menu;
    QHash<QString, int> m_lastWarned;
};
