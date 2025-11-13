# zmqc

`zmqc` 是一個使用 Rust 語言編寫的輕量型 ZeroMQ (ZMQ) 命令列測試工具，採用 **純 Rust** 的 `zeromq` 與 `tokio` 非同步架構，支援 **發布者 (Publisher)** 與 **訂閱者 (Subscriber)** 模式。它非常適合用於除錯、測試 ZMQ 網路拓撲，以及快速驗證 PUB/SUB 訊息傳遞。

## 💡 功能特色

*   **純 Rust 實現**：基於 `zeromq` 與 `tokio`，無需安裝或編譯 C/C++ `libzmq` 開發庫，亦無需依賴 `libzmq.dll`。
*   **非同步高效能 I/O**：全非同步事件驅動架構，通訊與磁碟 I/O 全面解耦。
*   **雙重模式支援**：可作為發布者（PUB）或訂閱者（SUB）運作。
*   **靈活的連線方式**：支援綁定端點（`--bind`）或連線端點（預設）。
*   **訊息主題過濾**：發布或接收時支援透過主題（`--topic`）進行過濾。
*   **發送流量節流**：二進位檔案串流支援 `--batch-size` 與 `--throttle-interval-ms` 彈性調控。
*   **檔案串流處理**：
    *   **發布模式**：可讀取檔案（`--file`）內容並發送。若為 UTF-8 文字，則逐行讀取；若為二進位檔案，則解析自定義二進位格式串流。
    *   **訂閱模式**：可將接收到的訊息寫入指定檔案（`--file`），具有 Tokio MPSC 佇列寫入、背景定期同步（每 3 秒自動 flush）、覆蓋保護提示，以及 Ctrl+C 優雅退出（排空記憶體緩衝後 flush）。
*   **Hex 降級顯示**：對於非 UTF-8 的二進位訊息，自動轉換為十六進位（Hex）字串輸出。

---

## 🛠️ 安裝與建置

### 前置需求

本專案為 **100% 純 Rust** 實作，**不需要**在作業系統中預先安裝 ZeroMQ C 語言函式庫或設定動態連結庫。只要有標準 Rust 開發環境即可：

*   Rust 2024 Edition (Rust 1.85+)
*   Cargo

### 建置專案

在專案目錄下執行：
```bash
cargo build --release
```
建置完成後，執行檔將位於：
*   **Linux / macOS**: `target/release/zmqc`
*   **Windows**: `target\release\zmqc.exe`

### 🚀 執行編譯後的執行檔（免 cargo）

編譯完成後，即可直接執行二進位檔，無需透過 `cargo run`：

#### 1. 直接以相對路徑執行
*   **Linux / macOS**：
    ```bash
    ./target/release/zmqc --help
    ```
*   **Windows (PowerShell / CMD)**：
    ```powershell
    .\target\release\zmqc.exe --help
    ```

#### 2. 加入系統 PATH（推薦全域使用）
*   **Linux / macOS**：將執行檔複製至 `/usr/local/bin`：
    ```bash
    sudo cp target/release/zmqc /usr/local/bin/
    ```
*   **Windows**：
    將 `target\release` 目錄加入系統環境變數 `Path`，或將 `zmqc.exe` 放置於已有 PATH 的目錄中。

---

## 📖 命令列參數說明

執行 `zmqc --help` 可查看完整的參數說明。以下為主要參數：

| 參數 | 類型 | 預設值 | 說明 |
| :--- | :--- | :--- | :--- |
| `--mode <pub\|sub>` | 必填 | - | 運作模式：`pub`（發布）或 `sub`（訂閱），不區分大小寫。 |
| `--endpoint <url>` | 必填 | - | ZeroMQ 端點位址。例如 `tcp://127.0.0.1:5555`。 |
| `--bind` | 開關 | `false` | 若加上此參數，Socket 會執行 `bind`；預設為 `connect`。 |
| `--topic <string>` | 選填 | `*` | 訊息主題。**PUB 模式**：不允許為空字串或 `*`（未指定時預設為 `*` 並提示輸入主題）；**SUB 模式**：主題開頭為 `*` 時自動解析為 `""` 接收全部資料。 |
| `--file <path>` | 選填 | - | **PUB 模式**：要讀取並發送的檔案路徑；**SUB 模式**：要寫入接收訊息的檔案路徑。 |
| `--batch-size <int>` | 選填 | `1000` | 二進位檔案串流發送的批次大小。 |
| `--throttle-interval-ms <int>` | 選填 | `100` | 二進位檔案串流每發送一個批次後非同步暫停之毫秒數（設為 0 則不暫停）。 |

---

## 📂 檔案模式詳細說明

### 1. PUB 模式：讀取檔案發送
當指定 `--file <path>` 時，`zmqc` 會自動探測該檔案的編碼：
*   **UTF-8 文字檔案**：逐行讀取檔案，並將每一行作為獨立訊息發送至指定主題。
*   **自定義二進位檔案**：若偵測到非 UTF-8 字元，會解析為以下格式串流：
    *   **Header（檔頭）**:
        *   `active` (1 byte)
        *   `seq_lock` (4 bytes, Little-Endian)
        *   `data_offset` (4 bytes, Little-Endian) - 指示資料結束的偏移植。
    *   **Body（資料區塊，迴圈讀取直到 offset >= data_offset）**:
        *   `data_len` (4 bytes, Little-Endian) - 當前封包長度。
        *   `data` (`data_len` bytes) - 封包內容，會作為 Socket 訊息發送。
        *   `bin_times` (8 bytes) - 時間戳記或保留欄位。
    *   *註：每發送達到 `--batch-size`（預設 1000）筆資料時，會非同步暫停 `--throttle-interval-ms`（預設 100ms），避免訂閱端佇列爆量掉包。*

### 2. SUB 模式：寫入檔案儲存
當指定 `--file <path>` 時：
*   若檔案已存在，會提示使用者確認是否覆蓋：`Overwrite? (y/N):`。
*   採用 Tokio MPSC Channel 非同步佇列將訊息傳送給獨立背景任務寫檔，網路接收不受磁碟延遲阻塞。
*   背景任務**每 3 秒自動執行 `flush`**，確保資料在長時間接收下定期持久化。
*   寫入格式為：`[{接收序號}] {主題} => {訊息內容}`。
*   支援 **Ctrl+C 優雅關閉**：收到中斷訊號時，會完整寫出佇列中所有待存訊息並 `flush` 後安全結束。

---

## 💻 使用範例

> 💡 **提示**：以下範例直接使用編譯後的執行檔 `zmqc` 進行說明。若尚未加入系統 PATH，請在專案根目錄下使用相對路徑：
> * **Linux / macOS**: `./target/release/zmqc`
> * **Windows**: `.\target\release\zmqc.exe`

### 範例 1：基本 Pub/Sub 測試（透過 TCP）

1.  **啟動訂閱者**，連線到 `tcp://127.0.0.1:5555` 並訂閱名為 `sensor` 的主題：
    ```bash
    zmqc --mode sub --endpoint tcp://127.0.0.1:5555 --topic sensor
    ```

2.  **啟動發布者**，並綁定到相同端點以推送訊息：
    ```bash
    zmqc --mode pub --endpoint tcp://127.0.0.1:5555 --bind --topic sensor
    ```
    啟動後，直接在發布者終端機輸入訊息並按下 Enter，訂閱者端即可即時收到訊息。

### 範例 2：將訂閱內容儲存至檔案
```bash
zmqc --mode sub --endpoint tcp://127.0.0.1:5555 --topic chat --file received_chat.log
```

### 範例 3：發布整個文字檔案的內容
```bash
zmqc --mode pub --endpoint tcp://127.0.0.1:5555 --bind --topic my_topic --file my_data.txt
```

---

## 🛠️ 開發維護常用命令

在維護專案相依套件時，建議使用以下工具：

*   **檢查未使用的相依套件**：
    ```bash
    cargo machete
    ```
*   **將相依套件更新至最新穩定版本**：
    ```bash
    cargo upgrade
    ```
