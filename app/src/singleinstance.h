#pragma once

#include <QObject>

// Keeps one app per session. The first instance owns a name on the session
// bus; a later launch hands its request to that instance over D-Bus and exits,
// so starting Peridot again (from the menu, at login, after a restart) brings
// the running window forward instead of opening a second one.
class SingleInstance : public QObject {
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "io.github.peridot.App")
public:
    explicit SingleInstance(QObject *parent = nullptr);

    // True when this process is now the only instance. False when another one
    // is running; the request has then been passed on to it (unless minimized,
    // which asks for nothing) and this process should exit.
    bool claim(bool minimized, const QString &device, const QString &section);

public slots:
    Q_SCRIPTABLE void Activate(const QString &device, const QString &section);

signals:
    void activateRequested(const QString &device, const QString &section);
};
