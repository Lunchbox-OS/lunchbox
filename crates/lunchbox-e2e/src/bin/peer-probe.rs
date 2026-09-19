//! Connect to lunchboxd's management socket and try one method, raw.
//!
//! Deliberately not `IpcClient`: that verifies the *server's* cgroup before it
//! speaks (issue #144), so from a foreign cgroup it would refuse the daemon and
//! never reach the daemon's own allow-list — proving the call fails, but not
//! that the server refused it. A raw socket is what puts the server's decision
//! under test.
//!
//! Prints exactly one line: `ACCEPTED`, `REFUSED`, or `ERROR <what>`.
//!
//!     peer-probe <socket-path> <method>

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(sock), Some(method)) = (args.next(), args.next()) else {
        println!("ERROR usage: peer-probe <socket-path> <method>");
        std::process::exit(2);
    };

    let mut stream = match UnixStream::connect(&sock) {
        Ok(s) => s,
        // The allow-list closes the connection at accept, which can surface
        // either here or on the first read depending on timing.
        Err(e) => {
            println!("REFUSED connect: {e}");
            return;
        }
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));

    let req = format!(r#"{{"request_id":1,"api_version":1,"method":"{method}","params":null}}"#);
    if let Err(e) = writeln!(stream, "{req}") {
        println!("REFUSED write: {e}");
        return;
    }

    let mut line = String::new();
    match BufReader::new(&stream).read_line(&mut line) {
        // A refusal closes without answering; a live daemon always answers.
        Ok(0) => println!("REFUSED closed with no response"),
        Ok(_) => println!("ACCEPTED {}", line.trim()),
        Err(e) => println!("REFUSED read: {e}"),
    }
}
