/// A 4x4 transformation matrix (column-major, like glam).
#[derive(Clone, Copy, Debug)]
pub struct Transform {
    /// Column-major storage: [col0, col1, col2, col3].
    pub cols: [[f64; 4]; 4],
}

impl Default for Transform {
    fn default() -> Self {
        Self::identity()
    }
}

impl Transform {
    pub fn identity() -> Self {
        Self {
            cols: [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
        }
    }

    /// Create a transform from AXIS2_PLACEMENT_3D components.
    pub fn from_axis2_placement(
        location: [f64; 3],
        axis: [f64; 3],
        ref_dir: [f64; 3],
    ) -> Self {
        // axis is Z direction, ref_dir is X direction.
        // Y = Z cross X.
        let z = normalize(axis);
        let x = normalize(ref_dir);
        let y = cross(z, x);

        Self {
            cols: [
                [x[0], x[1], x[2], 0.0],
                [y[0], y[1], y[2], 0.0],
                [z[0], z[1], z[2], 0.0],
                [location[0], location[1], location[2], 1.0],
            ],
        }
    }

    /// Multiply two transforms: self * other.
    pub fn mul(&self, other: &Transform) -> Transform {
        let mut result = [[0.0; 4]; 4];
        for (i, result_col) in result.iter_mut().enumerate() {
            for (j, result_elem) in result_col.iter_mut().enumerate() {
                for k in 0..4 {
                    *result_elem += self.cols[k][j] * other.cols[i][k];
                }
            }
        }
        Transform { cols: result }
    }

    /// Compute the inverse transform.
    pub fn inverse(&self) -> Transform {
        // For a rigid transform (rotation + translation), inverse is:
        // R^-1 = R^T, t^-1 = -R^T * t.
        let r00 = self.cols[0][0];
        let r01 = self.cols[1][0];
        let r02 = self.cols[2][0];
        let r10 = self.cols[0][1];
        let r11 = self.cols[1][1];
        let r12 = self.cols[2][1];
        let r20 = self.cols[0][2];
        let r21 = self.cols[1][2];
        let r22 = self.cols[2][2];
        let tx = self.cols[3][0];
        let ty = self.cols[3][1];
        let tz = self.cols[3][2];

        // R^T.
        let inv_tx = -(r00 * tx + r10 * ty + r20 * tz);
        let inv_ty = -(r01 * tx + r11 * ty + r21 * tz);
        let inv_tz = -(r02 * tx + r12 * ty + r22 * tz);

        Transform {
            cols: [
                [r00, r01, r02, 0.0],
                [r10, r11, r12, 0.0],
                [r20, r21, r22, 0.0],
                [inv_tx, inv_ty, inv_tz, 1.0],
            ],
        }
    }

    /// Transform a point.
    pub fn transform_point(&self, p: [f64; 3]) -> [f64; 3] {
        [
            self.cols[0][0] * p[0]
                + self.cols[1][0] * p[1]
                + self.cols[2][0] * p[2]
                + self.cols[3][0],
            self.cols[0][1] * p[0]
                + self.cols[1][1] * p[1]
                + self.cols[2][1] * p[2]
                + self.cols[3][1],
            self.cols[0][2] * p[0]
                + self.cols[1][2] * p[1]
                + self.cols[2][2] * p[2]
                + self.cols[3][2],
        ]
    }

    /// Transform a normal vector (rotation only, no translation).
    pub fn transform_normal(&self, n: [f64; 3]) -> [f64; 3] {
        [
            self.cols[0][0] * n[0]
                + self.cols[1][0] * n[1]
                + self.cols[2][0] * n[2],
            self.cols[0][1] * n[0]
                + self.cols[1][1] * n[1]
                + self.cols[2][1] * n[2],
            self.cols[0][2] * n[0]
                + self.cols[1][2] * n[1]
                + self.cols[2][2] * n[2],
        ]
    }
}

pub(crate) fn normalize(v: [f64; 3]) -> [f64; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len < 1e-10 {
        [0.0, 0.0, 1.0]
    } else {
        [v[0] / len, v[1] / len, v[2] / len]
    }
}

pub(crate) fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
