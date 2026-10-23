#include "tray.h"
#include "daemonclient.h"

#include <QAction>
#include <QDBusConnection>
#include <QDBusMessage>
#include <QGuiApplication>
#include <QMenu>
#include <QPainter>
#include <QPainterPath>
#include <QStyleHints>

// Thresholds that trigger a low-battery notification, highest first.
static const int kWarnLevels[] = {20, 10, 5};

static QString batteryText(const QVariantMap &d)
{
    if (!d.value("online").toBool())
        return QObject::tr("asleep");
    const QVariantMap b = d.value("battery").toMap();
    if (b.isEmpty() || b.value("percent").isNull())
        return QObject::tr("connected");
    QString t = QStringLiteral("%1%").arg(b.value("percent").toInt());
    if (b.value("charging").toBool())
        t += QObject::tr(", charging");
    return t;
}

Tray::Tray(DaemonClient *client, QObject *parent)
    : QObject(parent)
    , m_client(client)
    , m_tray(new QSystemTrayIcon(this))
    , m_menu(new QMenu())
{
    m_tray->setContextMenu(m_menu);
    m_tray->setToolTip(tr("Peridot"));
    connect(m_tray, &QSystemTrayIcon::activated, this, [this](QSystemTrayIcon::ActivationReason r) {
        if (r == QSystemTrayIcon::Trigger || r == QSystemTrayIcon::DoubleClick)
            emit showRequested(QString());
    });
    connect(client, &DaemonClient::devicesChanged, this, &Tray::update);
    connect(client, &DaemonClient::availableChanged, this, &Tray::update);
    connect(QGuiApplication::styleHints(), &QStyleHints::colorSchemeChanged, this, &Tray::update);
    update();
    m_tray->show();
}

bool Tray::isAvailable() const
{
    return QSystemTrayIcon::isSystemTrayAvailable();
}

QIcon Tray::renderIcon(int percent, bool charging, bool available) const
{
    const bool dark = QGuiApplication::styleHints()->colorScheme() != Qt::ColorScheme::Light;
    QIcon icon;
    for (int size : {16, 22, 24, 32, 48, 64}) {
        QPixmap pm(size, size);
        pm.fill(Qt::transparent);
        QPainter p(&pm);
        p.setRenderHint(QPainter::Antialiasing);
        const qreal w = qMax(2.0, size / 7.0);
        const QRectF r(w / 2 + 0.5, w / 2 + 0.5, size - w - 1, size - w - 1);
        QColor track = dark ? QColor(255, 255, 255, 70) : QColor(0, 0, 0, 60);
        p.setPen(QPen(track, w, Qt::SolidLine, Qt::FlatCap));
        p.drawEllipse(r);
        if (available && percent >= 0) {
            QColor c = percent <= 10 ? QColor("#e0533d") : percent <= 25 ? QColor("#e0a33d") : QColor("#8da33c");
            p.setPen(QPen(c, w, Qt::SolidLine, Qt::RoundCap));
            p.drawArc(r, 90 * 16, -int(360 * 16 * qBound(2, percent, 100) / 100.0));
        }
        // centre: a small gem when idle, a bolt when charging
        const QPointF c = r.center();
        const qreal s = size * 0.22;
        QPainterPath path;
        if (charging) {
            path.moveTo(c.x() + s * 0.2, c.y() - s * 1.1);
            path.lineTo(c.x() - s * 0.6, c.y() + s * 0.15);
            path.lineTo(c.x(), c.y() + s * 0.15);
            path.lineTo(c.x() - s * 0.2, c.y() + s * 1.1);
            path.lineTo(c.x() + s * 0.6, c.y() - s * 0.15);
            path.lineTo(c.x(), c.y() - s * 0.15);
            path.closeSubpath();
        } else {
            path.moveTo(c.x(), c.y() - s);
            path.lineTo(c.x() + s * 0.8, c.y() - s * 0.2);
            path.lineTo(c.x(), c.y() + s);
            path.lineTo(c.x() - s * 0.8, c.y() - s * 0.2);
            path.closeSubpath();
        }
        p.setPen(Qt::NoPen);
        p.setBrush(dark ? QColor("#e8ece6") : QColor("#2a3340"));
        p.drawPath(path);
        icon.addPixmap(pm);
    }
    return icon;
}

void Tray::notify(const QString &title, const QString &body, const QString &icon)
{
    QDBusMessage m = QDBusMessage::createMethodCall("org.freedesktop.Notifications", "/org/freedesktop/Notifications",
                                                    "org.freedesktop.Notifications", "Notify");
    QVariantMap hints;
    hints.insert("desktop-entry", "io.github.peridot");
    hints.insert("urgency", QVariant::fromValue<uchar>(1));
    m.setArguments({QStringLiteral("Peridot"), 0u, icon, title, body, QStringList(), hints, 8000});
    if (!QDBusConnection::sessionBus().send(m))
        m_tray->showMessage(title, body, QSystemTrayIcon::Warning, 8000);
}

void Tray::checkBatteries(const QVariantList &devices)
{
    for (const QVariant &v : devices) {
        const QVariantMap d = v.toMap();
        const QString id = d.value("id").toString();
        const QVariantMap b = d.value("battery").toMap();
        if (b.isEmpty() || b.value("percent").isNull() || !d.value("online").toBool())
            continue;
        const int pct = b.value("percent").toInt();
        if (b.value("charging").toBool() || pct > 25) {
            m_lastWarned.remove(id);
            continue;
        }
        int level = -1;
        for (int l : kWarnLevels)
            if (pct <= l)
                level = l;
        if (level < 0 || m_lastWarned.value(id, 101) <= level)
            continue;
        m_lastWarned.insert(id, level);
        const QString name = d.value("name").toString();
        const QString kind = d.value("kind").toString();
        notify(tr("%1 battery at %2%").arg(name).arg(pct),
               level <= 5 ? tr("Charge it now, it will switch off soon.") : tr("Plug it in when you get a chance."),
               kind == "headset" ? "audio-headset" : "input-mouse");
    }
}

void Tray::update()
{
    const QVariantList devices = m_client->devices();
    m_menu->clear();
    int lowest = -1;
    bool lowestCharging = false;
    QStringList tip;
    if (!m_client->available()) {
        m_menu->addAction(tr("Peridot service isn't running"))->setEnabled(false);
    } else if (devices.isEmpty()) {
        m_menu->addAction(tr("No devices connected"))->setEnabled(false);
    }
    for (const QVariant &v : devices) {
        const QVariantMap d = v.toMap();
        const QString id = d.value("id").toString();
        const QString line = QStringLiteral("%1 — %2").arg(d.value("name").toString(), batteryText(d));
        QAction *a = m_menu->addAction(line);
        connect(a, &QAction::triggered, this, [this, id] { emit showRequested(id); });
        tip << line;
        const QVariantMap b = d.value("battery").toMap();
        if (d.value("online").toBool() && !b.isEmpty() && !b.value("percent").isNull()) {
            const int p = b.value("percent").toInt();
            if (lowest < 0 || p < lowest) {
                lowest = p;
                lowestCharging = b.value("charging").toBool();
            }
        }
    }
    m_menu->addSeparator();
    connect(m_menu->addAction(tr("Open Peridot")), &QAction::triggered, this, [this] { emit showRequested(QString()); });
    connect(m_menu->addAction(tr("Quit")), &QAction::triggered, this, &Tray::quitRequested);
    m_tray->setIcon(renderIcon(lowest, lowestCharging, m_client->available()));
    m_tray->setToolTip(tip.isEmpty() ? tr("Peridot") : tip.join('\n'));
    checkBatteries(devices);
}
