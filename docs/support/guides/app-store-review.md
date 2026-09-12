# App Store / Google Play 审核材料（R27）

> iOS 3.3.2 / 2.5.2 与 Android 热更政策的合规姿态：**自有主机远程终端**。
> App 不提供云服务、账号、支付或内容分发；配对是用户对自己电脑的显式授权。

## 1. 审核话术（Review Notes 模板）

- R-Code Remote 是用户**自己电脑**上运行的 R-Code 开发工具的远程查看器。
- 不含账号体系、不含内购、不含第三方内容；所有数据只在用户设备与用户
  电脑之间点对点传输。
- 配对流程：用户在电脑端生成一次性配对码（120 秒有效），在 App 内输入或
  扫码完成绑定；此后通过证书钉扎的加密通道通信。
- 审核演示：需要一台运行 R-Code 桌面版的电脑 + 手机在同一网络。
  （提供演示视频：配对 → 查看任务 → 审批工具调用 → 吊销设备。）

## 2. iOS（App Store）

- **3.3.2（解释器）**：App 不下载、不执行任何非 Apple 签名的代码。UI 由
  App Bundle 内资源渲染（React Native bundle 随 App 签名分发，无 CodePush/
  Expo Updates/热更）。
- **2.5.2（通用链接/热更）**：不下载可执行代码；JS bundle 无远程来源。
- Info.plist 用途串：
  - `NSCameraUsageDescription` = "扫描电脑端显示的配对二维码，用于连接你
    自己的电脑。"
  - （无定位/麦克风/相册权限。）
- ATS：仅 wss/https（`NSAllowsArbitraryLoads` = NO）。

## 3. Android（Google Play）

- `android:usesCleartextTraffic="false"`；network security config 仅允许
  wss/https。
- 不使用动态特性/插件下载执行代码；不使用 JSPatch 类热更。
- 相机权限运行时请求，拒绝时降级手动输入配对。

## 4. 隐私清单

- 不收集任何数据（无遥测、无分析、无第三方 SDK 上报）。
- 数据仅在用户设备与用户电脑之间传输（E2EE）。

## 5. 签名与构建

- 证书/Keystore **不进仓库**（文档指引：本地 Keychain / CI secrets）。
- CI 构建签名：iOS 用 Xcode 自动签名（开发者账号），Android 用
  `debug` keystore 验证流程；release keystore 由用户保管。

## 6. 截图清单（审核材料）

1. 配对屏（手动输入 + 扫码入口）。
2. 任务列表（深色 obsidian 皮肤）。
3. 会话实时流。
4. 审批卡片（批准/拒绝）。
5. 设置/设备信息（能力只读 + 指纹核对）。
