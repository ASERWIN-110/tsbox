# tsbox 使用手册

`tsbox` 是一个跨平台 CLI 工具，用来：

- 把任意文件打包进合法 MPEG-TS 容器。
- 从 TSBOX 文件恢复原始文件。
- 从普通媒体 `.ts/.m2ts` 中导出 MP4 或原始 elementary streams。

## 构建

```bash
cargo build --release
```

可执行文件：

```text
target/release/tsbox
```

## 基本命令

打包单个文件：

```bash
tsbox pack file.zip
```

输出：

```text
file.ts
```

解包 TSBOX 文件：

```bash
tsbox extract file.ts
```

输出 basename 规则：

```text
file.ts -> file.zip
```

普通媒体 TS 默认 remux 为 MP4：

```bash
tsbox extract movie.ts
```

输出：

```text
movie.mp4
```

## 批处理

处理目录：

```bash
tsbox pack ./input -o ./packed
tsbox extract ./packed -o ./out
```

递归处理：

```bash
tsbox pack ./input -o ./packed --recursive
tsbox extract ./packed -o ./out --recursive
```

并发处理：

```bash
tsbox pack ./input -o ./packed --jobs 4
tsbox extract ./packed -o ./out --jobs 4
```

关闭进度输出：

```bash
tsbox extract ./packed -o ./out --quiet
```

## 删除源文件

成功处理一个文件后删除一个源文件：

```bash
tsbox pack ./input -o ./packed --delete-source
tsbox extract ./packed -o ./out --delete-source
```

安全规则：

- 输出先写入临时文件。
- 校验和提交成功后才删除源文件。
- 单个文件失败时不会删除该源文件。
- 批处理中其他文件仍会继续处理。

## Raw Demux

不使用 ffmpeg，直接导出原始流：

```bash
tsbox extract video.ts --raw -o ./raw
```

支持的 raw stream：

```text
H.264       -> .h264
H.265/HEVC  -> .h265
AAC         -> .aac
MP3         -> .mp3
AC3         -> .ac3
E-AC3       -> .eac3
MPEG video  -> .m2v
DVB subtitle-> .dvbsub
Teletext    -> .teletext
LPCM        -> .lpcm
```

单流输出：

```text
video.ts -> video.h264
```

多流或多节目输出：

```text
video_p1_pid0100.h264
video_p1_pid0101.aac
video_p2_pid0120.h265
```

## MP4 失败时回退 Raw

默认媒体模式会调用 ffmpeg：

```bash
tsbox extract video.ts -o ./out
```

如果希望 ffmpeg remux 失败时自动改为 raw 导出：

```bash
tsbox extract video.ts -o ./out --fallback raw
```

## 错误处理

工具会拒绝以下情况：

- 输出文件已经存在。
- 临时输出文件已经存在。
- 批量输入会映射到同一个输出 basename。
- TS raw 流声明存在，但没有实际 payload。
- TS packet transport error indicator 置位。
- 同一个 PID 的 continuity counter 不连续。
- PES header 不完整或非法。

失败时行为：

- 不覆盖已有文件。
- 不提交空输出。
- 不删除失败源文件。
- 清理未提交的临时文件。

## 压力测试

建议用大文件验证 TSBOX 打包/解包：

```bash
dd if=/dev/zero of=/tmp/tsbox_128m.bin bs=1M count=128
tsbox pack /tmp/tsbox_128m.bin -o /tmp/tsbox_pack
tsbox extract /tmp/tsbox_pack/tsbox_128m.ts -o /tmp/tsbox_out
sha256sum /tmp/tsbox_128m.bin /tmp/tsbox_out/tsbox_128m.bin
```

## 本地 Release 包

```bash
scripts/build_release.sh
```

产物目录：

```text
dist/
```

Linux 本机包会直接生成。Windows/macOS 交叉构建取决于本机是否安装对应 target 和 linker。GitHub Actions 配置会在真实平台 runner 上构建多平台 release。
