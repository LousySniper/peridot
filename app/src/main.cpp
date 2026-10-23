#include "daemonclient.h"
#include "tray.h"

#include <QApplication>
#include <QCommandLineParser>
#include <QIcon>
#include <QQmlApplicationEngine>
#include <QQmlContext>
#include <QQuickStyle>
#include <QQuickWindow>
#include <QStyleHints>
#include <QTimer>

int main(int argc, char *argv[])
{
    QApplication app(argc, argv);
    app.setApplicationName("peridot");
    app.setOrganizationName("peridot"); // settings in ~/.config/peridot/peridot.conf
    app.setApplicationDisplayName("Peridot");
    app.setApplicationVersion("0.2.0");
    app.setDesktopFileName("io.github.peridot");
    app.setOrganizationDomain("peridot.github.io");
    app.setQuitOnLastWindowClosed(false);
    app.setWindowIcon(QIcon::fromTheme("io.github.peridot", QIcon::fromTheme("input-mouse")));

    QCommandLineParser cli;
    cli.setApplicationDescription("Settings for Logitech and Razer mice and headsets");
    cli.addHelpOption();
    cli.addVersionOption();
    QCommandLineOption minimized("minimized", "Start in the system tray without opening the window.");
    QCommandLineOption select("device", "Open on the device whose id or name contains <text>.", "text");
    QCommandLineOption section("section", "Open on a section (sensitivity, lighting, buttons, sound, power, about).", "name");
    QCommandLineOption shot("screenshot", "Render the window to <file> and exit (for documentation).", "file");
    QCommandLineOption scheme("color-scheme", "Force the light or dark look instead of following the desktop.", "light|dark");
    QCommandLineOption size("window-size", "Open the window at this size, e.g. 1000x900 (for screenshots).", "WxH");
    cli.addOptions({minimized, select, section, shot, scheme, size});
    cli.process(app);
#if QT_VERSION >= QT_VERSION_CHECK(6, 8, 0)
    if (cli.value(scheme) == "light")
        app.styleHints()->setColorScheme(Qt::ColorScheme::Light);
    else if (cli.value(scheme) == "dark")
        app.styleHints()->setColorScheme(Qt::ColorScheme::Dark);
#endif

    QQuickStyle::setStyle("Basic");

    DaemonClient client;
    Tray tray(&client);
    const bool screenshot = cli.isSet(shot);

    QQmlApplicationEngine engine;
    engine.rootContext()->setContextProperty("Daemon", &client);
    engine.rootContext()->setContextProperty("TrayAvailable", tray.isAvailable() && !screenshot);
    engine.rootContext()->setContextProperty("InitialDevice", cli.value(select));
    engine.rootContext()->setContextProperty("InitialSection", cli.value(section));
    engine.rootContext()->setContextProperty("ForcedScheme", cli.value(scheme));
    QObject::connect(&engine, &QQmlApplicationEngine::objectCreationFailed, &app, [] { QCoreApplication::exit(1); },
                     Qt::QueuedConnection);
    engine.loadFromModule("Peridot", "Main");
    if (engine.rootObjects().isEmpty())
        return 1;
    auto *window = qobject_cast<QQuickWindow *>(engine.rootObjects().first());
    if (const QStringList wh = cli.value(size).split('x'); wh.size() == 2 && wh[0].toInt() > 0 && wh[1].toInt() > 0)
        window->resize(wh[0].toInt(), wh[1].toInt());

    auto show = [window](const QString &id) {
        if (!id.isEmpty())
            QMetaObject::invokeMethod(window, "selectDevice", Q_ARG(QVariant, id));
        window->show();
        window->raise();
        window->requestActivate();
    };
    QObject::connect(&tray, &Tray::showRequested, window, show);
    QObject::connect(&tray, &Tray::quitRequested, &app, &QCoreApplication::quit);

    if (screenshot) {
        window->show();
        QTimer::singleShot(2500, window, [window, &cli, &shot] {
            window->grabWindow().save(cli.value(shot));
            QCoreApplication::quit();
        });
    } else if (!cli.isSet(minimized) || !tray.isAvailable()) {
        window->show();
    }
    return app.exec();
}
