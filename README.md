# zmqc

[English](#english) | [繁體中文](#繁體中文)

---

## English

`zmqc` is a lightweight ZeroMQ (ZMQ) command-line testing utility written in Rust. Powered by a **pure Rust** architecture using `zeromq` and `tokio` async runtime, it supports **Publisher (PUB)**, **Subscriber (SUB)**, **Dealer**, and **Router** modes. It is ideal for debugging, testing ZMQ network topologies, and rapidly verifying message exchanges.

### 💡 Features

*   **Pure Rust Implementation**: Built on `zeromq` and `tokio`—no need to install or compile C/C++ `libzmq` development libraries, with zero dependency on `libzmq.dll`.
*   **Asynchronous High-Performance I/O**: Fully asynchronous, event-driven architecture with complete decoupling of network communication and disk I/O.
*   **Multiple Socket Modes**: Supports Publisher (PUB), Subscriber (SUB), Dealer, and Router modes.
*   **Flexible Connection Types**: Supports binding to an endpoint (`--bind`) or connecting to an endpoint (default).
*   **Topic Filtering**: Supports filtering messages by topic (`--topic`) when publishing or subscribing.
*   **Throughput Throttling**: Binary file streaming supports fine-grained rate limiting via `--batch-size` and `--throttle-interval-ms`.
*   **File Streaming**:
    *   **Publish Mode**: Read and publish file contents (`--file`). Reads line-by-line for UTF-8 text files; parses custom binary stream format for binary files.
    *   **Subscribe Mode**: Write received messages to a specified file (`--file`), featuring Tokio MPSC queued writing, periodic background flushing (every 3 seconds), overwrite protection prompts, and graceful shutdown on Ctrl+C (draining in-memory buffers before flushing).
*   **Hex Fallback Display**: Automatically converts non-UTF-8 binary payloads into hexadecimal (Hex) string representations for display.

---

### 🛠️ Installation & Building

#### Prerequisites

This project is a **100% pure Rust** implementation. It **does not require** pre-installing the C ZeroMQ library or configuring dynamic link libraries on your operating system. A standard Rust development toolchain is all you need:

*   Rust 2024 Edition (Rust 1.85+)
*   Cargo

#### Building the Project

Run the following command in the project directory:
```bash
cargo build --release
```
After the build completes, the executable will be located at:
*   **Linux / macOS**: `target/release/zmqc`
*   **Windows**: `target\release\zmqc.exe`

#### 🚀 Running the Compiled Binary (Without Cargo)

Once compiled, you can run the binary directly without `cargo run`:

##### 1. Run Directly via Relative Path
*   **Linux / macOS**:
    ```bash
    ./target/release/zmqc --help
    ```
*   **Windows (PowerShell / CMD)**:
    ```powershell
    .\target\release\zmqc.exe --help
    ```

##### 2. Add to System PATH (Recommended for Global Use)
*   **Linux / macOS**: Copy the executable to `/usr/local/bin`:
    ```bash
    sudo cp target/release/zmqc /usr/local/bin/
    ```
*   **Windows**:
    Add the `target\release` directory to your system's `Path` environment variable, or move `zmqc.exe` to an existing directory in your PATH.

---

### 📖 Command-Line Options

Run `zmqc --help` to view full help details. Below are the primary arguments:

| Option | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `--mode <pub\|sub\|dealer\|router>` | Required | - | Operating mode: `pub`, `sub`, `dealer`, or `router` (case-insensitive). |
| `--endpoint <url>` | Required | - | ZeroMQ endpoint address (e.g. `tcp://127.0.0.1:5555`). |
| `--bind` | Flag | `false` | If specified, the socket executes `bind`; defaults to `connect` (**ROUTER mode is always forced to bind**). |
| `--topic <string>` | Optional | `*` | Message topic. **PUB mode**: empty string or `*` is not allowed; **SUB mode**: topic starting with `*` automatically resolves to `""` to receive all messages; **DEALER/ROUTER mode**: `*` or `""` is treated as an empty topic frame, otherwise sends the specified topic frame. |
| `--file <path>` | Optional | - | **PUB mode**: file path to read and publish; **SUB mode**: file path to write received messages (**not supported in DEALER/ROUTER**). |
| `--identity <string>` | Optional | - | **DEALER / ROUTER mode**: Specify custom ZeroMQ Socket Identity. |
| `--ack` | Flag | `false` | **ROUTER mode**: Automatically reply `ACK` to the source peer upon receiving any message. |
| `--batch-size <int>` | Optional | `1000` | Batch size for binary file streaming. |
| `--throttle-interval-ms <int>` | Optional | `100` | Delay in milliseconds between batches during binary file streaming (0 disables throttling). |

---

### 📂 Detailed Mode Guide

#### 1. PUB Mode: Publish from File or stdin
When `--file <path>` is specified, `zmqc` automatically detects the file encoding:
*   **UTF-8 Text File**: Reads line-by-line and publishes each line as an independent message under the specified topic.
*   **Custom Binary File**: If non-UTF-8 bytes are detected, it parses the content as a Header + Body stream, throttled by `--batch-size` and `--throttle-interval-ms`.
*   If `--file` is omitted, `zmqc` reads lines from stdin and publishes them in real-time.

#### 2. SUB Mode: Receive and Save to File
*   Receives and prints messages in real time in the format: `[{index}] {topic} => {payload}`.
*   If `--file <path>` is specified, incoming messages are written to disk via an asynchronous queue, automatically flushed every 3 seconds, with file overwrite protection and graceful buffer flushing upon Ctrl+C.

#### 3. DEALER / ROUTER Mode: Bidirectional Interactive Communication
Both `dealer` and `router` are **bidirectional modes** (capable of simultaneous sending and receiving):
*   **Message Frame Structure**:
    *   **Dealer**: Sends and receives fixed 2 frames: `[topic, payload]`. If topic is `*` or `""`, topic is sent as an empty frame.
    *   **Router**: Receives and sends fixed 3 frames: `[identity, topic, payload]`.
*   **Dealer Interactive Behavior**:
    *   Each line typed into stdin is sent immediately. In the background, dealer asynchronously receives Router replies and prints: `[{index}] {topic} => {payload}`.
    *   Specify a custom identity via `--identity <name>` for easy identification by the Router.
*   **Router Interactive Behavior**:
    *   **Always Binds**: Automatically listens via `bind` (ignores `--bind` flag).
    *   Receives messages from Dealers and prints: `[{index}] {identity} {topic} => {payload}` (identities that are not valid UTF-8 are displayed as lowercase hex).
    *   **Target Peer Routing**:
        *   Enter `<message>` in stdin: Sent to the **most recently active peer** by default.
        *   Enter `<message>|<identity>` (split on the last `|` delimiter): Explicitly targets a specific peer identity (matches textual identity first, then hex).
        *   If no peer has connected yet or the specified identity is unknown, the message is **silently dropped** without terminating the process.
    *   **Automatic ACK (`--ack`)**:
        *   When enabled, Router automatically responds with `[identity, "" (empty topic), "ACK"]` upon receiving any message.
        *   `--ack` can be used simultaneously with manual sending via stdin.
*   **stdin EOF Protection**:
    *   When piped input ends (e.g. `echo msg | zmqc --mode dealer ...`), the program does not terminate prematurely. It displays a notification and continues listening for replies in the background until Ctrl+C is pressed.

---

### 💻 Usage Examples

> 💡 **Tip**: The following examples use the compiled `zmqc` binary directly. If it has not been added to your system PATH, use relative paths from the project root:
> * **Linux / macOS**: `./target/release/zmqc`
> * **Windows**: `.\target\release\zmqc.exe`

#### Example 1: Basic Pub/Sub Testing (via TCP)

1.  **Start Subscriber**, connect to `tcp://127.0.0.1:5555` and subscribe to all topics:
    ```bash
    zmqc --mode sub --endpoint tcp://127.0.0.1:5555
    ```

2.  **Start Publisher**, bind to the same endpoint and broadcast messages:
    ```bash
    zmqc --mode pub --endpoint tcp://127.0.0.1:5555 --bind --topic sensor
    ```

#### Example 2: Save Subscribed Messages to File
```bash
zmqc --mode sub --endpoint tcp://127.0.0.1:5555 --topic chat --file received_chat.log
```

#### Example 3: Publish Text File Content
```bash
zmqc --mode pub --endpoint tcp://127.0.0.1:5555 --bind --topic my_topic --file my_data.txt
```

#### Example 4: Dealer & Router Bidirectional Communication with Auto-ACK

1.  **Start Router** (automatically binds to endpoint with auto-ACK enabled):
    ```bash
    zmqc --mode router --endpoint tcp://127.0.0.1:5560 --ack
    ```

2.  **Start Dealer** (set identity to `alice`):
    ```bash
    zmqc --mode dealer --endpoint tcp://127.0.0.1:5560 --identity alice --topic chat
    ```
    Type `hello` in the Dealer console. The Router receives `[1] alice chat => hello`, and the Dealer immediately receives the auto-ACK response `[1]  => ACK`.

3.  **Router Replies to a Specific Peer**:
    Type `hi alice|alice` in the Router console, and the message will be precisely routed to `alice`.

---

### 🛠️ Development & Maintenance Commands

Recommended tools for maintaining project dependencies:

*   **Check for unused dependencies**:
    ```bash
    cargo machete
    ```
*   **Upgrade dependencies to latest stable versions**:
    ```bash
    cargo upgrade
    ```

---

## 繁體中文

`zmqc` 是一個使用 Rust 語言編寫的輕量型 ZeroMQ (ZMQ) 命令列測試工具，採用 **純 Rust** 的 `zeromq` 與 `tokio` 非同步架構，支援 **發布者 (Publisher)**、**訂閱者 (Subscriber)**、**Dealer** 與 **Router** 模式。它非常適合用於除錯、測試 ZMQ 網路拓撲，以及快速驗證訊息傳遞。

### 💡 功能特色

*   **純 Rust 實現**：基於 `zeromq` 與 `tokio`，無需安裝或編譯 C/C++ `libzmq` 開發庫，亦無需依賴 `libzmq.dll`。
*   **非同步高效能 I/O**：全非同步事件驅動架構，通訊與磁碟 I/O 全面解耦。
*   **多種模式支援**：支援發布者（PUB）、訂閱者（SUB）、Dealer 與 Router 運作模式。
*   **靈活的連線方式**：支援綁定端點（`--bind`）或連線端點（預設）。
*   **訊息主題過濾**：發布或接收時支援透過主題（`--topic`）進行過濾。
*   **發送流量節流**：二進位檔案串流支援 `--batch-size` 與 `--throttle-interval-ms` 彈性調控。
*   **檔案串流處理**：
    *   **發布模式**：可讀取檔案（`--file`）內容並發送。若為 UTF-8 文字，則逐行讀取；若為二進位檔案，則解析自定義二進位格式串流。
    *   **訂閱模式**：可將接收到的訊息寫入指定檔案（`--file`），具有 Tokio MPSC 佇列寫入、背景定期同步（每 3 秒自動 flush）、覆蓋保護提示，以及 Ctrl+C 優雅退出（排空記憶體緩衝後 flush）。
*   **Hex 降級顯示**：對於非 UTF-8 的二進位訊息，自動轉換為十六進位（Hex）字串輸出。

---

### 🛠️ 安裝與建置

#### 前置需求

本專案為 **100% 純 Rust** 實作，**不需要**在作業系統中預先安裝 ZeroMQ C 語言函式庫或設定動態連結庫。只要有標準 Rust 開發環境即可：

*   Rust 2024 Edition (Rust 1.85+)
*   Cargo

#### 建置專案

在專案目錄下執行：
```bash
cargo build --release
```
建置完成後，執行檔將位於：
*   **Linux / macOS**: `target/release/zmqc`
*   **Windows**: `target\release\zmqc.exe`

#### 🚀 執行編譯後的執行檔（免 cargo）

編譯完成後，即可直接執行二進位檔，無需透過 `cargo run`：

##### 1. 直接以相對路徑執行
*   **Linux / macOS**：
    ```bash
    ./target/release/zmqc --help
    ```
*   **Windows (PowerShell / CMD)**：
    ```powershell
    .\target\release\zmqc.exe --help
    ```

##### 2. 加入系統 PATH（推薦全域使用）
*   **Linux / macOS**：將執行檔複製至 `/usr/local/bin`：
    ```bash
    sudo cp target/release/zmqc /usr/local/bin/
    ```
*   **Windows**：
    將 `target\release` 目錄加入系統環境變數 `Path`，或將 `zmqc.exe` 放置於已有 PATH 的目錄中。

---

### 📖 命令列參數說明

執行 `zmqc --help` 可查看完整的參數說明。以下為主要參數：

| 參數 | 類型 | 預設值 | 說明 |
| :--- | :--- | :--- | :--- |
| `--mode <pub\|sub\|dealer\|router>` | 必填 | - | 運作模式：`pub`、`sub`、`dealer` 或 `router`，不區分大小寫。 |
| `--endpoint <url>` | 必填 | - | ZeroMQ 端點位址。例如 `tcp://127.0.0.1:5555`。 |
| `--bind` | 開關 | `false` | 若加上此參數，Socket 會執行 `bind`；預設為 `connect`（**ROUTER 模式永遠強制為 bind**）。 |
| `--topic <string>` | 選填 | `*` | 訊息主題。**PUB 模式**：不允許為空字串或 `*`；**SUB 模式**：主題開頭為 `*` 時自動解析為 `""` 接收全部資料；**DEALER/ROUTER 模式**：`*` 或 `""` 視為空 topic frame，否則帶入指定 topic frame。 |
| `--file <path>` | 選填 | - | **PUB 模式**：要讀取並發送的檔案路徑；**SUB 模式**：要寫入接收訊息的檔案路徑（**DEALER/ROUTER 不支援**）。 |
| `--identity <string>` | 選填 | - | **DEALER / ROUTER 模式**：指定自己的 ZeroMQ Socket Identity。 |
| `--ack` | 開關 | `false` | **ROUTER 模式**：收到任何訊息時自動回覆 `ACK` 給來源 Peer。 |
| `--batch-size <int>` | 選填 | `1000` | 二進位檔案串流發送的批次大小。 |
| `--throttle-interval-ms <int>` | 選填 | `100` | 二進位檔案串流每發送一個批次後非同步暫停之毫秒數（設為 0 則不暫停）。 |

---

### 📂 運作模式詳細說明

#### 1. PUB 模式：讀取檔案或 stdin 發送
當指定 `--file <path>` 時，`zmqc` 會自動探測該檔案的編碼：
*   **UTF-8 文字檔案**：逐行讀取檔案，並將每一行作為獨立訊息發送至指定主題。
*   **自定義二進位檔案**：若偵測到非 UTF-8 字元，會解析為 Header + Body 串流，並依 `--batch-size` 與 `--throttle-interval-ms` 節流。
*   若未指定 `--file`，則從 stdin 讀取每行訊息即時發布。

#### 2. SUB 模式：接收與寫入檔案儲存
*   接收訊息並即時印出，格式為：`[{接收序號}] {主題} => {訊息內容}`。
*   若指定 `--file <path>`，會透過非同步佇列寫入檔案，每 3 秒自動 `flush`，並支援覆蓋確認與 Ctrl+C 安全排空。

#### 3. DEALER / ROUTER 模式：雙向互動通訊
`dealer` 與 `router` 均為**雙向模式**（可同時發送與接收）：
*   **訊息 Frame 結構**：
    *   **Dealer**：送/收固定 2 Frames `[topic, payload]`。若 topic 為 `*` 或 `""` 則 topic 為 empty frame。
    *   **Router**：收/送固定 3 Frames `[identity, topic, payload]`。
*   **Dealer 互動行為**：
    *   終端機 stdin 每輸入一行即發送，背景同時非同步接收 Router 的回覆並輸出：`[{序號}] {topic} => {payload}`。
    *   可透過 `--identity <name>` 指定自定義 Identity，方便 Router 辨識。
*   **Router 互動行為**：
    *   **永遠 Bind 監聽**（忽略 `--bind` 設定）。
    *   接收來自 Dealer 的訊息並輸出：`[{序號}] {identity} {topic} => {payload}`（Identity 若非 UTF-8 則顯示為小寫 Hex）。
    *   **發送對象指定**：
        *   直接在 stdin 輸入 `<message>`：預設送給**最近一次發送訊息來的 Peer**。
        *   輸入 `<message>|<identity>`（以最後一個 `|` 切分）：指定發送給特定的 Peer Identity（優先比對文字名稱，其次比對 Hex）。
        *   若尚未有任何 Peer 連線或指定的 Identity 不存在，訊息會**靜默丟棄**而不中斷程式。
    *   **自動 ACK (`--ack`)**：
        *   開啟後，Router 每收到一則訊息會立即自動回覆 `[identity, "" (空 topic), "ACK"]`。
        *   `--ack` 與 stdin 手動發送可同時並存使用。
*   **stdin 結束（EOF）保護**：
    *   當以管線輸入資料（例如 `echo msg | zmqc --mode dealer ...`）結束時，程式不會直接退出，而是提示並持續在背景接收回覆，直到按下 Ctrl+C 結束。

---

### 💻 使用範例

> 💡 **提示**：以下範例直接使用編譯後的執行檔 `zmqc` 進行說明。若尚未加入系統 PATH，請在專案根目錄下使用相對路徑：
> * **Linux / macOS**: `./target/release/zmqc`
> * **Windows**: `.\target\release\zmqc.exe`

#### 範例 1：基本 Pub/Sub 測試（透過 TCP）

1.  **啟動訂閱者**，連線到 `tcp://127.0.0.1:5555` 並訂閱所有主題：
    ```bash
    zmqc --mode sub --endpoint tcp://127.0.0.1:5555
    ```

2.  **啟動發布者**，並綁定到相同端點以推送訊息：
    ```bash
    zmqc --mode pub --endpoint tcp://127.0.0.1:5555 --bind --topic sensor
    ```

#### 範例 2：將訂閱內容儲存至檔案
```bash
zmqc --mode sub --endpoint tcp://127.0.0.1:5555 --topic chat --file received_chat.log
```

#### 範例 3：發布文字檔案內容
```bash
zmqc --mode pub --endpoint tcp://127.0.0.1:5555 --bind --topic my_topic --file my_data.txt
```

#### 範例 4：Dealer 與 Router 雙向通訊與自動 ACK

1.  **啟動 Router 端**（自動綁定端點，並開啟自動 ACK）：
    ```bash
    zmqc --mode router --endpoint tcp://127.0.0.1:5560 --ack
    ```

2.  **啟動 Dealer 端**（指定 identity 為 alice）：
    ```bash
    zmqc --mode dealer --endpoint tcp://127.0.0.1:5560 --identity alice --topic chat
    ```
    在 Dealer 輸入 `hello`，Router 端即會收到 `[1] alice chat => hello`，且 Dealer 端會立即收到自動回覆 `[1]  => ACK`。

3.  **Router 端指定回覆給特定 Peer**：
    在 Router 端輸入 `hi alice|alice`，訊息即會精準路由至 `alice` 端。

---

### 🛠️ 開發維護常用命令

在維護專案相依套件時，建議使用以下工具：

*   **檢查未使用的相依套件**：
    ```bash
    cargo machete
    ```
*   **將相依套件更新至最新穩定版本**：
    ```bash
    cargo upgrade
    ```
