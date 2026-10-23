import QtQuick
import QtQuick.Shapes

// Line drawings of a mouse or headset, sized to `size`.
Item {
    id: root
    property string kind: "mouse"
    property color color: Look.text
    property real size: 28
    implicitWidth: size
    implicitHeight: size

    Shape {
        anchors.fill: parent
        preferredRendererType: Shape.CurveRenderer
        scale: root.size / 24
        transformOrigin: Item.TopLeft

        ShapePath {
            strokeColor: root.color
            strokeWidth: 1.6
            fillColor: "transparent"
            capStyle: ShapePath.RoundCap
            joinStyle: ShapePath.RoundJoin
            PathSvg {
                path: root.kind === "headset"
                      ? "M4 15 V12 A8 8 0 0 1 20 12 V15 M4 14 H6.5 A1.5 1.5 0 0 1 8 15.5 V19.5 A1.5 1.5 0 0 1 6.5 21 H5.5 A1.5 1.5 0 0 1 4 19.5 Z M20 14 H17.5 A1.5 1.5 0 0 0 16 15.5 V19.5 A1.5 1.5 0 0 0 17.5 21 H18.5 A1.5 1.5 0 0 0 20 19.5 Z"
                      : root.kind === "keyboard"
                        ? "M3 7 H21 V17 H3 Z M6 10 H7 M9 10 H10 M12 10 H13 M15 10 H16 M18 10 H18.5 M7 14 H17"
                        : "M12 3 A6 6 0 0 1 18 9 V15 A6 6 0 0 1 6 15 V9 A6 6 0 0 1 12 3 Z M12 3 V9 M6 9 H18 M12 5.5 V7"
            }
        }
    }
}
