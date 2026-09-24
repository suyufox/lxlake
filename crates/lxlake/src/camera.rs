//! 自由飞行相机：M1 唯一的输入消费方，无碰撞、无重力。
//!
//! 与输入契约的分界：相机**不认识按键、不认识窗口事件**，只吃已经映射好的轴值。
//! 「W → `forward = 1`」这类映射属于应用 / 输入层，相机只管「轴值 + 鼠标位移 → 变换」。
//! 这样改键位、加手柄、将来做 HUD 都碰不到相机代码。
//!
//! 数学边界一律用 `[f32; N]` / 列主序矩阵这层 POD——相机不把任何数学库的类型漏到接口上。
//! 内部算矩阵用 glam，出了这个文件就只剩数组。

use glam::camera::rh::{proj::directx, view};
use glam::{Mat4, Vec3};

/// 按住加速键时的速度倍率。
const BOOST_MULTIPLIER: f32 = 4.0;

/// 俯仰的极限：差一点点到正负九十度，免得 up 向量与视线共线、视图矩阵退化。
const PITCH_LIMIT: f32 = std::f32::consts::FRAC_PI_2 - 0.001;

/// 相机本帧的输入快照。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CameraInput {
  /// 前后轴：−1 后退 / +1 前进。
  pub forward: f32,
  /// 左右轴：−1 左移 / +1 右移。
  pub right: f32,
  /// 上下轴：−1 下降 / +1 上升。
  pub up: f32,
  /// 加速（Shift 那类）。
  pub boost: bool,
  /// 鼠标位移（像素）：x 转 yaw，y 转 pitch。
  pub look: [f32; 2],
}

/// 自由飞行相机。
pub struct FlyCamera {
  position: [f32; 3],
  /// 偏航（弧度），绕 Y 轴。
  yaw: f32,
  /// 俯仰（弧度），夹在 ±(π/2 − ε) 内以免翻滚。
  pitch: f32,
  /// 基础速度（方块 / 秒）。
  speed: f32,
  /// 鼠标灵敏度（弧度 / 像素）。
  sensitivity: f32,
}

impl FlyCamera {
  /// 近平面。
  pub const NEAR: f32 = 0.1;
  /// 远平面。
  pub const FAR: f32 = 1000.0;
  /// 纵向视场角（70°）。
  pub const FOV_Y: f32 = 70.0 * std::f32::consts::PI / 180.0;

  pub fn new(position: [f32; 3]) -> Self {
    Self {
      position,
      yaw: 0.0,
      pitch: 0.0,
      speed: 24.0,
      sensitivity: 0.002_5,
    }
  }

  pub fn position(&self) -> [f32; 3] {
    self.position
  }

  pub fn set_position(&mut self, position: [f32; 3]) {
    self.position = position;
  }

  /// 偏航（弧度）。
  pub fn yaw(&self) -> f32 {
    self.yaw
  }

  /// 俯仰（弧度）。
  pub fn pitch(&self) -> f32 {
    self.pitch
  }

  /// 基础速度（方块 / 秒）。
  pub fn speed(&self) -> f32 {
    self.speed
  }

  pub fn set_speed(&mut self, speed: f32) {
    self.speed = speed;
  }

  /// 鼠标灵敏度（弧度 / 像素）。
  pub fn sensitivity(&self) -> f32 {
    self.sensitivity
  }

  pub fn set_sensitivity(&mut self, sensitivity: f32) {
    self.sensitivity = sensitivity;
  }

  /// 每帧推进：先吃掉本帧的鼠标位移（转 yaw / pitch），再按轴值平移。
  ///
  /// `dt` 是秒——位移按时间积分，帧率高低不影响飞行手感。
  pub fn update(&mut self, input: &CameraInput, dt: f32) {
    self.yaw += input.look[0] * self.sensitivity;
    // 屏幕 y 向下为正，而 pitch 向上为正，所以这里是减。
    self.pitch = (self.pitch - input.look[1] * self.sensitivity).clamp(-PITCH_LIMIT, PITCH_LIMIT);

    let axis =
      self.forward_vec() * input.forward + self.right_vec() * input.right + Vec3::Y * input.up;
    let speed = if input.boost {
      self.speed * BOOST_MULTIPLIER
    } else {
      self.speed
    };

    // 归一化后再乘速度：斜着飞（前进 + 平移）不会比直着飞更快。
    if let Some(direction) = axis.try_normalize() {
      let moved = Vec3::from_array(self.position) + direction * speed * dt;
      self.position = moved.to_array();
    }
  }

  /// 前方向单位向量。
  pub fn forward(&self) -> [f32; 3] {
    self.forward_vec().to_array()
  }

  /// 右方向单位向量。
  pub fn right(&self) -> [f32; 3] {
    self.right_vec().to_array()
  }

  /// 视图矩阵（列主序）。
  pub fn view(&self) -> [[f32; 4]; 4] {
    let eye = Vec3::from_array(self.position);
    view::look_to_mat4(eye, self.forward_vec(), Vec3::Y).to_cols_array_2d()
  }

  /// 观察投影矩阵（列主序），`aspect = 宽 / 高`。
  pub fn view_projection(&self, aspect: f32) -> [[f32; 4]; 4] {
    // directx 那套 = 「Z 在 0..1、Y 朝上」，正是 wgpu 的 NDC（vulkan 那套 Y 是朝下的）。
    let projection =
      directx::perspective(Self::FOV_Y, aspect.max(f32::EPSILON), Self::NEAR, Self::FAR);
    let view = Mat4::from_cols_array_2d(&self.view());
    (projection * view).to_cols_array_2d()
  }

  /// 视线方向（含俯仰）。
  ///
  /// 约定：`yaw = 0, pitch = 0` 时朝 `-Z`；yaw 增大朝 `+X` 转，pitch 增大抬头。
  fn forward_vec(&self) -> Vec3 {
    let (sin_yaw, cos_yaw) = self.yaw.sin_cos();
    let (sin_pitch, cos_pitch) = self.pitch.sin_cos();
    Vec3::new(sin_yaw * cos_pitch, sin_pitch, -cos_yaw * cos_pitch)
  }

  /// 右方向：视线叉世界向上。
  fn right_vec(&self) -> Vec3 {
    // 俯仰被夹在正负九十度之内，这里的叉乘不会退化成零向量。
    self.forward_vec().cross(Vec3::Y).normalize()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-5
  }

  fn close3(a: [f32; 3], b: [f32; 3]) -> bool {
    close(a[0], b[0]) && close(a[1], b[1]) && close(a[2], b[2])
  }

  /// 只转不飞：`dt = 0`，位置不该动。
  fn look(camera: &mut FlyCamera, dx: f32, dy: f32) {
    camera.update(
      &CameraInput {
        look: [dx, dy],
        ..CameraInput::default()
      },
      0.0,
    );
  }

  /// 转出指定的 yaw（弧度），走真实路径。
  fn turn_to_yaw(camera: &mut FlyCamera, yaw: f32) {
    look(camera, yaw / camera.sensitivity(), 0.0);
  }

  #[test]
  fn default_view_looks_down_negative_z() {
    let camera = FlyCamera::new([0.0; 3]);
    assert!(close3(camera.forward(), [0.0, 0.0, -1.0]));
    assert!(close3(camera.right(), [1.0, 0.0, 0.0]));
  }

  #[test]
  fn moving_mouse_right_turns_view_right() {
    let mut camera = FlyCamera::new([0.0; 3]);
    turn_to_yaw(&mut camera, std::f32::consts::FRAC_PI_2);
    assert!(close(camera.yaw(), std::f32::consts::FRAC_PI_2));
    assert!(close3(camera.forward(), [1.0, 0.0, 0.0]));
  }

  #[test]
  fn moving_mouse_down_looks_down() {
    let mut camera = FlyCamera::new([0.0; 3]);
    look(&mut camera, 0.0, 100.0);
    assert!(camera.pitch() < 0.0, "鼠标往下移该低头");
    assert!(close3(
      camera.forward(),
      [0.0, camera.pitch().sin(), -camera.pitch().cos()]
    ));
  }

  #[test]
  fn pitch_is_clamped_short_of_vertical() {
    let mut camera = FlyCamera::new([0.0; 3]);
    look(&mut camera, 0.0, 1.0e6);
    assert!(camera.pitch() > -std::f32::consts::FRAC_PI_2);
    assert!(close(camera.pitch(), -PITCH_LIMIT));

    look(&mut camera, 0.0, -2.0e6);
    assert!(close(camera.pitch(), PITCH_LIMIT));
  }

  #[test]
  fn forward_axis_moves_along_the_look_direction() {
    let mut camera = FlyCamera::new([1.0, 2.0, 3.0]);
    let speed = camera.speed();
    camera.update(
      &CameraInput {
        forward: 1.0,
        ..CameraInput::default()
      },
      1.0,
    );
    assert!(close3(camera.position(), [1.0, 2.0, 3.0 - speed]));
  }

  #[test]
  fn boost_multiplies_speed() {
    let mut camera = FlyCamera::new([0.0; 3]);
    let speed = camera.speed();
    camera.update(
      &CameraInput {
        forward: 1.0,
        boost: true,
        ..CameraInput::default()
      },
      1.0,
    );
    assert!(close(camera.position()[2], -speed * BOOST_MULTIPLIER));
  }

  #[test]
  fn diagonal_axes_do_not_move_faster() {
    let mut camera = FlyCamera::new([0.0; 3]);
    let speed = camera.speed();
    camera.update(
      &CameraInput {
        forward: 1.0,
        right: 1.0,
        ..CameraInput::default()
      },
      1.0,
    );

    let moved = Vec3::from_array(camera.position()).length();
    assert!(close(moved, speed), "斜飞与直飞同速");
  }

  #[test]
  fn up_axis_ignores_pitch() {
    let mut camera = FlyCamera::new([0.0; 3]);
    look(&mut camera, 0.0, 200.0);
    camera.update(
      &CameraInput {
        up: 1.0,
        ..CameraInput::default()
      },
      1.0,
    );
    let position = camera.position();
    assert!(close(position[0], 0.0) && close(position[2], 0.0));
    assert!(close(position[1], camera.speed()));
  }

  #[test]
  fn view_projection_puts_the_point_ahead_inside_the_clip_range() {
    let camera = FlyCamera::new([0.0, 0.0, 0.0]);
    let vp = Mat4::from_cols_array_2d(&camera.view_projection(16.0 / 9.0));

    let ahead = Vec3::from_array(camera.position()) + Vec3::from_array(camera.forward()) * 10.0;
    let clip = vp * ahead.extend(1.0);

    assert!(clip.w > 0.0, "身前的点该在近平面之外");
    let depth = clip.z / clip.w;
    assert!(
      (0.0..=1.0).contains(&depth),
      "wgpu 的深度区间是 0..1，实得 {depth}"
    );
  }
}
