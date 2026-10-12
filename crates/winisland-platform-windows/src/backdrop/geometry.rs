use windows::Graphics::{IGeometrySource2D, IGeometrySource2D_Impl};
use windows::UI::Composition::{
    CompositionBrush, CompositionGeometricClip, CompositionPath, CompositionPathGeometry,
    Compositor, ContainerVisual, SpriteVisual,
};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_BEZIER_SEGMENT, D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_END_CLOSED, D2D1_FILL_MODE_ALTERNATE,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_FACTORY_TYPE_MULTI_THREADED, D2D1_QUADRATIC_BEZIER_SEGMENT, D2D1CreateFactory,
    ID2D1Factory, ID2D1Geometry,
};
use windows::Win32::System::WinRT::Graphics::Direct2D::{
    IGeometrySource2DInterop, IGeometrySource2DInterop_Impl,
};
use windows::core::{Interface, Ref, Result};
use windows_numerics::{Vector2, Vector3};
use winisland_platform::ClipSegment;

use super::ShapeGeometry;

pub(super) struct PathVisual {
    visual: SpriteVisual,
    _clip: CompositionGeometricClip,
    geometry: CompositionPathGeometry,
    factory: ID2D1Factory,
}

impl PathVisual {
    pub(super) fn new(
        compositor: &Compositor,
        brush: &CompositionBrush,
        root: &ContainerVisual,
    ) -> Result<Self> {
        let visual = compositor.CreateSpriteVisual()?;
        let geometry = compositor.CreatePathGeometry()?;
        let clip = compositor.CreateGeometricClipWithGeometry(&geometry)?;
        visual.SetBrush(brush)?;
        visual.SetClip(&clip)?;
        visual.SetIsVisible(false)?;
        root.Children()?.InsertAtTop(&visual)?;
        // SAFETY: The factory is retained and only used to build device-independent geometries.
        let factory = unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_MULTI_THREADED, None)? };
        Ok(Self {
            visual,
            _clip: clip,
            geometry,
            factory,
        })
    }

    pub(super) fn apply(
        &self,
        shape: Option<ShapeGeometry>,
        segments: &[ClipSegment],
    ) -> Result<()> {
        let Some(shape) = shape else {
            return self.hide();
        };
        // SAFETY: The retained factory owns the sink and all segment arrays are valid for these calls.
        let path = unsafe {
            let path = self.factory.CreatePathGeometry()?;
            let sink = path.Open()?;
            sink.SetFillMode(D2D1_FILL_MODE_ALTERNATE);
            let p = |xy: [f32; 2]| Vector2 { X: xy[0], Y: xy[1] };
            for segment in segments {
                match *segment {
                    ClipSegment::Move(point) => {
                        sink.BeginFigure(p(point), D2D1_FIGURE_BEGIN_FILLED)
                    }
                    ClipSegment::Line(point) => sink.AddLine(p(point)),
                    ClipSegment::Quadratic(control, end) => {
                        sink.AddQuadraticBezier(&D2D1_QUADRATIC_BEZIER_SEGMENT {
                            point1: p(control),
                            point2: p(end),
                        })
                    }
                    ClipSegment::Cubic(c1, c2, end) => sink.AddBezier(&D2D1_BEZIER_SEGMENT {
                        point1: p(c1),
                        point2: p(c2),
                        point3: p(end),
                    }),
                    ClipSegment::Close => sink.EndFigure(D2D1_FIGURE_END_CLOSED),
                }
            }
            sink.Close()?;
            path
        };
        let source: IGeometrySource2D = GeometrySource {
            geometry: path.cast()?,
        }
        .into();
        self.geometry.SetPath(&CompositionPath::Create(&source)?)?;
        self.visual.SetOffset(Vector3 {
            X: shape.x,
            Y: shape.y,
            Z: 0.0,
        })?;
        self.visual.SetSize(Vector2 {
            X: shape.width,
            Y: shape.height,
        })?;
        self.visual.SetOpacity(shape.opacity)?;
        self.visual.SetIsVisible(true)
    }

    pub(super) fn hide(&self) -> Result<()> {
        self.visual.SetIsVisible(false)
    }

    pub(super) fn set_brush(&self, brush: &CompositionBrush) -> Result<()> {
        self.visual.SetBrush(brush)
    }
}

#[windows::core::implement(IGeometrySource2D, IGeometrySource2DInterop)]
struct GeometrySource {
    geometry: ID2D1Geometry,
}

impl IGeometrySource2D_Impl for GeometrySource_Impl {}

impl IGeometrySource2DInterop_Impl for GeometrySource_Impl {
    fn GetGeometry(&self) -> Result<ID2D1Geometry> {
        Ok(self.geometry.clone())
    }
    fn TryGetGeometryUsingFactory(&self, factory: Ref<'_, ID2D1Factory>) -> Result<ID2D1Geometry> {
        let factory = factory.ok()?;
        // SAFETY: Both factories and the copied geometry are retained throughout the stream operation.
        unsafe {
            let path = factory.CreatePathGeometry()?;
            let sink = path.Open()?;
            self.geometry.Simplify(windows::Win32::Graphics::Direct2D::D2D1_GEOMETRY_SIMPLIFICATION_OPTION_CUBICS_AND_LINES, None, 0.1, &sink)?;
            sink.Close()?;
            path.cast()
        }
    }
}
