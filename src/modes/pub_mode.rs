use std::io::{self, BufRead, BufReader, Read, Seek};
use zmq;

/// Sets up a PUB socket, binds or connects, and publishes lines from stdin.
pub fn run_publisher(args: crate::ZmqArgs) {
    let ctx = zmq::Context::new();
    let socket = ctx.socket(zmq::PUB).expect("Failed to create PUB socket");

    if let Some(buf) = args.buf {
        socket.set_sndbuf(buf).expect("Failed to set ZMQ_SNDBUF");
    }

    let hwm = match args.hwm {
        Some(_hwm) => {
            socket.set_sndhwm(_hwm).expect("Failed to set ZMQ_SNDHWM");
            _hwm
        }
        None => socket.get_sndhwm().unwrap_or(1000),
    };

    let bind_flag = args.bind;
    let endpoint = &args.endpoint;
    if bind_flag {
        socket.bind(endpoint).expect("Failed to bind PUB socket");
        println!("PUB bound to {}", endpoint);
    } else {
        socket
            .connect(endpoint)
            .expect("Failed to connect PUB socket");
        println!("PUB connected to {}", endpoint);
    }

    std::thread::sleep(std::time::Duration::from_millis(500));

    let topic = args.topic.as_deref();
    let mut buf = String::new();
    let topic_str = if topic.is_none() {
        println!("Enter topic to publish (Ctrl+C to exit):");
        io::stdin()
            .read_line(&mut buf)
            .expect("Failed to read topic");
        buf.trim()
    } else {
        topic.expect("Topic should be provided in PUB mode")
    };

    // TODO 新增使用 模式 => 複製二近位檔案
    if let Some(input_file) = args.file {
        // publish messages from the import file first
        let mut file = std::fs::File::open(&input_file).expect("Failed to open import file");

        // 先用 file 直接讀出前面的 bytes 判斷是否為文字 (避免同時有兩個可變借用)
        let mut probe_buf = [0u8; 1024];
        let n = file.read(&mut probe_buf).unwrap_or(0);

        // 將游標回到開頭，然後把 file 的所有權移入 BufReader
        file.seek(io::SeekFrom::Start(0)).unwrap();

        if std::str::from_utf8(&probe_buf[..n]).is_ok() {
            let reader = io::BufReader::new(file);

            for line in reader.lines() {
                let message = line.expect("Failed to read string line from import file");

                if message.len() != 0 {
                    socket
                        .send(topic_str, zmq::SNDMORE)
                        .expect("Failed to send envelope");
                    socket.send(&message, 0).expect("Failed to send message");
                }
            }
        } else {
            let mut reader = BufReader::new(file);

            let mut active_buf = [0u8; 1];
            let _ = match reader.read_exact(&mut active_buf) {
                Ok(_) => active_buf[0] != 0,
                _ => false,
            };

            let mut seq_lock_buf = [0u8; 4];
            let _ = match reader.read_exact(&mut seq_lock_buf) {
                Ok(_) => i32::from_le_bytes(seq_lock_buf),
                _ => 0,
            };

            let mut data_offset_buf = [0u8; 4];
            let data_offset = match reader.read_exact(&mut data_offset_buf) {
                Ok(_) => i32::from_le_bytes(data_offset_buf),
                _ => 0,
            } as usize;

            // 9
            let mut offset = active_buf.len() + seq_lock_buf.len() + data_offset_buf.len();
            let mut send_count = 0;
            let mut total_count = 0;
            let mut last_buf = vec![0u8; 2048];
            loop {
                if offset >= data_offset {
                    break;
                }

                let mut data_len_buf = [0u8; 4];
                let data_len = match reader.read_exact(&mut data_len_buf) {
                    Ok(_) => i32::from_le_bytes(data_len_buf),
                    _ => 0,
                } as usize;

                let mut data_buf = vec![0u8; data_len];
                if let Ok(_) = reader.read_exact(&mut data_buf) {
                    socket
                        .send(topic_str, zmq::SNDMORE)
                        .expect("Failed to send envelope");
                    socket.send(&data_buf, 0).expect("Failed to send message");
                    send_count = send_count + 1;
                    total_count = total_count + 1;
                };
                last_buf = data_buf.clone();
                // let data_string = match reader.read_exact(&mut data_buf) {
                //     Ok(_) => match String::from_utf8(data_buf) {
                //         Ok(s) => s,
                //         Err(e) => e.to_string(),
                //     },
                //     Err(_) => "".to_string(),
                // };

                let mut bin_times_buf = [0u8; 8];
                let _ = reader.read_exact(&mut bin_times_buf);

                // println!(
                //     "[{}] {},{} => {}",
                //     data_offset, offset, data_len, data_string
                // );

                offset = offset + 4 + data_len + 8;

                if send_count >= hwm {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    send_count = 0;
                }
            }

            println!(
                "{} => {:?}",
                total_count,
                String::from_utf8(last_buf.to_vec())
            );
        }
    }

    let stdin = io::stdin();
    println!("Enter messages to publish (Ctrl+C to exit):");
    for line in stdin.lock().lines() {
        let message = line.expect("Failed to read from stdin");

        socket
            .send(topic_str, zmq::SNDMORE)
            .expect("Failed to send envelope");
        socket.send(&message, 0).expect("Failed to send message");
    }
}
