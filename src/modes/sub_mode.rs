use std::{
    io::{BufWriter, Write},
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use zmq::{self};

/// Sets up a SUB socket, binds or connects, subscribes to a topic (or all), and prints incoming messages.
pub fn run_subscriber(args: crate::ZmqArgs) {
    let ctx = zmq::Context::new();
    let socket = ctx.socket(zmq::SUB).expect("Failed to create SUB socket");

    if let Some(buf) = args.buf {
        socket.set_rcvbuf(buf).expect("Failed to set ZMQ_RCVBUF");
    }

    if let Some(hwm) = args.hwm {
        socket.set_rcvhwm(hwm).expect("Failed to set ZMQ_RCVHWM");
    }

    let endpoint = &args.endpoint;
    let bind_flag = args.bind;
    if bind_flag {
        socket.bind(endpoint).expect("Failed to bind SUB socket");
        println!("SUB bound to {}", endpoint);
    } else {
        socket
            .connect(endpoint)
            .expect("Failed to connect SUB socket");
        println!("SUB connected to {}", endpoint);
    }

    let topic = args.topic.as_deref();
    if let Some(topic_str) = topic {
        socket
            .set_subscribe(topic_str.as_bytes())
            .expect("Failed to subscribe to topic");
        println!("Subscribed to topic: {}", topic_str);
    } else {
        socket
            .set_subscribe(b"")
            .expect("Failed to subscribe to all");
        println!("Subscribed to all topics");
    }

    println!("Waiting for messages...");

    let mut recv_count = 0;

    if let Some(output_file) = args.file {
        // TODO 實作直接輸出至檔案，需要先檢查檔案是否存在，若存在則詢問是否覆蓋
        let path = std::path::Path::new(&output_file);
        if path.exists() {
            println!(
                "Output file '{}' already exists. Overwrite? (y/N):",
                output_file
            );
            let mut ans = String::new();
            std::io::stdin()
                .read_line(&mut ans)
                .expect("Failed to read answer");
            let a = ans.trim().to_lowercase();
            if a != "y" && a != "yes" {
                println!("Aborted by user.");
                return;
            }
        }

        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&output_file)
            .expect("Failed to open output file");
        // let mut writer = std::io::BufWriter::new(file);
        let writer = Arc::new(Mutex::new(BufWriter::new(file)));

        // 背景 thread 每 3 秒 flush
        {
            let writer_clone = Arc::clone(&writer);
            thread::spawn(move || {
                loop {
                    thread::sleep(Duration::from_secs(3));
                    let mut w = writer_clone.lock().unwrap();
                    let _ = w.flush();
                    // println!("Flushed in background");
                }
            });
        }

        loop {
            let mut topic_frame = zmq::Message::new();
            socket
                .recv(&mut topic_frame, 0)
                .expect("Failed to receive topic frame");
            let topic_disp = bytes_to_display(&topic_frame);

            let mut msg_frame = zmq::Message::new();
            socket
                .recv(&mut msg_frame, 0)
                .expect("Failed to receive message frame");
            let msg_disp = bytes_to_display(&msg_frame);

            recv_count = recv_count + 1;
            writeln!(
                writer.lock().unwrap(),
                "[{}] {} => {}",
                recv_count,
                topic_disp,
                msg_disp
            )
            .expect("Failed to write to file");
        }
    } else {
        loop {
            let mut topic_frame = zmq::Message::new();
            socket
                .recv(&mut topic_frame, 0)
                .expect("Failed to receive topic frame");
            let topic_disp = bytes_to_display(&topic_frame);

            let mut msg_frame = zmq::Message::new();
            socket
                .recv(&mut msg_frame, 0)
                .expect("Failed to receive message frame");
            let msg_disp = bytes_to_display(&msg_frame);

            recv_count = recv_count + 1;
            println!("[{}] {} => {}", recv_count, topic_disp, msg_disp);
        }
    }
}

fn bytes_to_display(data: &[u8]) -> String {
    match std::str::from_utf8(data) {
        Ok(s) => s.to_string(),
        Err(_) => data
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<String>(),
    }
}
