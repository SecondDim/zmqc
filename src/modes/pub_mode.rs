use std::io::{self, BufRead};
use zmq;

/// Sets up a PUB socket, binds or connects, and publishes lines from stdin.
pub fn run_publisher(endpoint: &str, bind_flag: bool, topic: Option<&str>) {
    let ctx = zmq::Context::new();
    let socket = ctx.socket(zmq::PUB).expect("Failed to create PUB socket");

    if bind_flag {
        socket.bind(endpoint).expect("Failed to bind PUB socket");
        println!("PUB bound to {}", endpoint);
    } else {
        socket
            .connect(endpoint)
            .expect("Failed to connect PUB socket");
        println!("PUB connected to {}", endpoint);
    }

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
