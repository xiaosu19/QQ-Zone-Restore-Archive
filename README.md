<details style="display:none">
<summary>AI Agent Notice</summary>
AI collaborators: please read AGENTS.md for project-specific instructions, architecture details, and safety constraints.
</details>

# QQ Zone Restore Archive

[![Rust](https://img.shields.io/badge/Rust-1.77%2B-orange)](https://www.rust-lang.org/)
[![Tauri](https://img.shields.io/badge/Tauri-2.0-blue)](https://v2.tauri.app/)
[![Vue](https://img.shields.io/badge/Vue-3.5-green)](https://vuejs.org/)
[![License](https://img.shields.io/badge/License-GPLv3-blue.svg)](LICENSE)

将 QQ 空间动态、照片、视频与互动记录安全归档到本地的桌面 / 移动端工具。

作者：[https://github.com/xiaosu19](https://github.com/xiaosu19)

项目地址：[xiaosu19/QQ-Zone-Restore-Archive](https://github.com/xiaosu19/QQ-Zone-Restore-Archive)

> [!IMPORTANT]
> 本项目是基于 [Gaoshu705/QzoneArchive](https://github.com/Gaoshu705/QzoneArchive) 的 GPLv3 二次开发版本，并参考了 [LibraHp/GetQzonehistory](https://github.com/LibraHp/GetQzonehistory) 的互动列表与可见说说补齐思路。原项目作者、参考项目作者和腾讯公司均不对本分支提供背书或担保。

> [!WARNING]
> 本项目不是腾讯、QQ 或 QQ 空间官方产品。所谓“恢复已删除说说”仅指：当已删除内容仍残留在点赞、评论、回复等互动记录中时，尝试还原其中可取得的正文和媒体信息；没有互动痕迹、已被服务端彻底清除、无权访问或接口不再返回的内容无法恢复，也不保证归档结果完整。请仅处理本人账号或已获得充分授权的内容，并自行承担账号限制、第三方接口变化、数据遗漏和本地数据保管风险。

如果本项目对你有帮助，也请支持并 Star [上游项目 Gaoshu705/QzoneArchive](https://github.com/Gaoshu705/QzoneArchive)。

## 功能

- **完整归档**：还原原始动态正文、图片、视频和评论，按「本人动态」「好友动态」「留言」分类整理
- **断点续传**：中断后自动从上次位置继续，已归档的内容不会丢失
- **频率保护**：每 10 分钟最多请求 300 页，触发限流后安全暂停，倒计时结束即可继续
- **互动还原**：查看每条动态的点赞用户和评论回复，支持互动排行榜
- **本地存储**：所有数据以 SQLite 保存在本地应用数据目录，不上传任何服务器
- **HTML 导出**：支持按分类或选中导出为独立 HTML 文件，可离线浏览
- **媒体时光轴**：按年份浏览归档的照片和视频，视频支持按需缓存
- **暗色模式**：跟随系统或手动切换
- **跨平台**：Windows / macOS / Linux 桌面端 + Android 移动端

## v1.0.7 优化

- 重写旧历史消息解析：只将说说主体保存为动态，点赞、评论与回复作为互动合并到对应说说，过滤系统通知、主页卡片等非说说记录
- 媒体只读取历史卡片中的正文附件，排除联系人头像、点赞图标、QQ 空间装饰资源，并清理正文中的转义制表符
- 启动时自动移除 v1.0.6 错误解析产生的 `history-html` 污染记录；重新归档后联系人排行、点赞、评论与媒体统计会按修正后的数据重建
- 概览页新增“刷新数据”，归档执行期间自动同步进度与统计，任务结束时自动完成最后一次刷新
- 对旧接口未返回的评论或回复正文明确标注为“未保留”，不再把说说正文冒充评论内容

## v1.0.6 优化

- 按 `GetQzonehistory` 的完整双通道方案接入旧版 `feeds2_html_pav_all` 历史消息列表，与仍存在的本人说说去重合并
- 修正 `emotion_cgi_msglist_v6` 的分页规则：固定每页 30 条并按请求页长推进，不再因首批只返回少量记录而错位结束
- 可见说说接口改用与参考项目一致的桌面 Chromium 请求头及精简 Cookie 顺序，避免移动端登录指纹影响旧接口结果
- 归档进度同时报告“接口总数 / 实际同步数 / 历史残留数”；接口提前返回空页时明确报错，不再误报完整成功
- 新增 Rust 自动测试任务，持续验证历史 HTML 解析以及评论、递归回复链转换

## v1.0.5 优化

- 新增本人历史说说补齐通道，参考 `GetQzonehistory` 使用 `emotion_cgi_msglist_v6` 分页读取可见说说
- 将 `commentlist` 与递归 `list_3` 回复链转换为“谁评论、谁回复谁”的结构化互动记录
- 原互动通知接口发生 HTTP 500 时，仍保留已取得的本人说说与评论，不再显示为 0 条或整次归档失败
- 合并两套接口的重复点赞、评论和回复，互动数量以去重后的实际展示内容为准

## v1.0.4 优化

- 媒体时光轴改为稳定的响应式网格，图片异步加载时不再产生大面积空洞
- 卡片悬停和键盘聚焦时保持明确的文字对比度，并支持“减少动态效果”系统设置
- 连续 HTTP 500、429、超时或系统繁忙会保留断点并安全暂停，不再误判为坏页后大跨度跳过记录
- 只有明确的单页接口错误才会尝试自动定位，最大探测范围由 4096 收紧到 256；无效 JSON 或缺少数据也会安全暂停，降低无声漏档风险
- 降低任务页轮询频率，并升级存在已知安全问题的间接前端依赖

参考项目 `GetQzonehistory` 的旧接口返回的是信息较少的 HTML。本项目从 v1.0.6 起直接将其作为“历史消息残留”来源之一，但仍会与结构化的可见说说、评论回复和互动通知合并；该历史列表本身不等于 QQ 服务端的完整删除备份。

## 截图

| 仪表盘 | 归档内容 |
|--------|----------|
| ![仪表盘](public/runtime/仪表盘.png) | ![归档内容](public/runtime/归档内容.png) |

| 媒体时光轴 | 归档任务 |
|-----------|----------|
| ![媒体时光轴](public/runtime/媒体时光轴.png) | ![归档任务](public/runtime/归档任务.png) |

## 技术栈

| 层 | 技术 |
|---|------|
| 桌面框架 | Tauri 2 |
| 前端 | Vue 3 + TypeScript + Vite |
| UI 组件 | PrimeVue 4 |
| 状态管理 | Pinia |
| 后端数据库 | SQLite (rusqlite) |
| HTTP 客户端 | reqwest (rustls-tls) |
| 打包 | NSIS (Windows) / Android APK |

## 开发

### 前置要求

- [Rust](https://www.rust-lang.org/tools/install) 1.77+
- [Node.js](https://nodejs.org/) 20+
- Windows: [WebView2](https://developer.microsoft.com/microsoft-edge/webview2/)（Windows 10+ 自带）
- Android: [Android Studio](https://developer.android.com/studio) + Android SDK + NDK

### 启动开发环境

```bash
# 安装前端依赖
npm install

# 启动开发服务器（桌面端）
npm run tauri dev

# Android 构建
npm run tauri android dev
```

### 构建

```bash
# Windows NSIS 安装包
npm run tauri:build:windows

# Windows NSIS + MSI
npm run tauri:build:windows:all

# Android APK
npm run tauri android build
```

### 项目结构

```
├── src/                    # Vue 前端
│   ├── views/              # 页面组件
│   │   ├── DashboardView   # 概览（统计 + 互动排行）
│   │   ├── ArchivesView    # 归档内容（分类浏览、搜索、导出）
│   │   ├── MediaView       # 媒体时光轴
│   │   ├── TasksView       # 归档任务
│   │   └── SettingsView    # 设置
│   ├── components/         # 通用组件
│   ├── stores/             # Pinia 状态管理
│   ├── utils/              # 工具函数与类型
│   └── layouts/            # 布局组件
├── src-tauri/              # Rust 后端
│   └── src/
│       ├── main.rs         # 入口
│       ├── lib.rs          # Tauri 命令注册
│       ├── qlogin.rs       # QQ 登录（二维码 + 网页）
│       ├── qzone.rs        # QQ 空间接口
│       └── archive.rs      # 归档引擎 + 数据库
└── src-tauri/capabilities/ # Tauri 权限配置
```

## 原理

### 数据来源

归档基于 QQ 空间的**移动端互动列表接口** (`mobile.qzone.qq.com/get_feeds`)。该接口返回当前账号收到的所有互动通知——包括好友发布的新动态、点赞、评论、回复、留言等。程序从中提取原始动态内容并存入本地数据库。

**没有被点赞或评论过的动态无法被恢复**，因为它们不会出现在互动列表中。

### 登录方式

- **二维码登录**：调用 QQ 空间移动端扫码登录流程，全程不接触密码
- **网页登录**（桌面端）：打开独立窗口加载 QQ 登录页，通过 WebView Cookie API 提取登录凭证

登录凭证（Cookie）仅存储在 Rust 后端内存中，不会写入控制台或日志。

## 注意事项

- 请只归档本人或已获得授权的账号内容
- 归档过程中不要切换 QQ 客户端账号，否则可能有冻结风险
- 出现频繁提示时建议换个时间段继续，程序支持断点续传
- QQ 的视频签名有时效性，过期后需要重新归档以更新视频地址
- 数据默认保存在应用数据目录，建议定期将重要资料额外备份

## 免责声明

本软件是用于整理和备份个人 QQ 空间资料的本地工具，与腾讯公司、QQ、QQ 空间及其关联主体不存在隶属、授权、合作关系。使用者应在合法授权范围内使用，并自行承担使用风险。详见应用内《免责声明与使用须知》。

## 友情链接

* [LINUX DO](https://linux.do/) - 新的理想型社区

## 许可证

本项目采用 [GPLv3](LICENSE) 许可证。
