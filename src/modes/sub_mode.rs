use zmq::{self};

/// Sets up a SUB socket, binds or connects, subscribes to a topic (or all), and prints incoming messages.
pub fn run_subscriber(endpoint: &str, bind_flag: bool, topic: Option<&str>) {
    let ctx = zmq::Context::new();
    let socket = ctx.socket(zmq::SUB).expect("Failed to create SUB socket");

    if bind_flag {
        socket.bind(endpoint).expect("Failed to bind SUB socket");
        println!("SUB bound to {}", endpoint);
    } else {
        socket
            .connect(endpoint)
            .expect("Failed to connect SUB socket");
        println!("SUB connected to {}", endpoint);
    }

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

        println!("[>] {} => {}", topic_disp, msg_disp);
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
