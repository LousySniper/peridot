cmake_minimum_required(VERSION 3.21)
project(peridot-app VERSION 1.0.1 LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_AUTOMOC ON)

find_package(Qt6 6.6 REQUIRED COMPONENTS Core Gui Widgets Qml Quick QuickControls2 DBus Svg)
qt_standard_project_setup(REQUIRES 6.6)

qt_add_executable(peridot
    src/main.cpp
    src/daemonclient.cpp src/daemonclient.h
    src/tray.cpp src/tray.h
    src/singleinstance.cpp src/singleinstance.h
)

set_source_files_properties(qml/Look.qml PROPERTIES QT_QML_SINGLETON_TYPE TRUE)
# Serve the QML files from the module root so they share the module's qmldir
# (otherwise each file's implicit directory import hides the Look singleton).
file(GLOB PERIDOT_QML RELATIVE ${CMAKE_CURRENT_SOURCE_DIR} qml/*.qml)
foreach(f ${PERIDOT_QML})
    get_filename_component(name ${f} NAME)
    set_source_files_properties(${f} PROPERTIES QT_RESOURCE_ALIAS ${name})
endforeach()

qt_add_qml_module(peridot
    URI Peridot
    VERSION 1.0
    QML_FILES
        qml/Main.qml
        qml/Look.qml
        qml/DeviceList.qml
        qml/DevicePage.qml
        qml/EmptyState.qml
        qml/SectionTitle.qml
        qml/SettingRow.qml
        qml/Segmented.qml
        qml/PSwitch.qml
        qml/PSlider.qml
        qml/PButton.qml
        qml/BatteryBadge.qml
        qml/DeviceGlyph.qml
        qml/DpiRuler.qml
        qml/SensitivitySection.qml
        qml/LightingSection.qml
        qml/ColorPicker.qml
        qml/ButtonsSection.qml
        qml/ShortcutDialog.qml
        qml/SoundSection.qml
        qml/PowerSection.qml
        qml/AboutSection.qml
        qml/Toast.qml
)

target_link_libraries(peridot PRIVATE
    Qt6::Core Qt6::Gui Qt6::Widgets Qt6::Qml Qt6::Quick Qt6::QuickControls2 Qt6::DBus Qt6::Svg)

install(TARGETS peridot RUNTIME DESTINATION ${CMAKE_INSTALL_BINDIR})
