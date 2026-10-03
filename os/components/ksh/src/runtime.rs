use crate::input::{Event, Input};
use kcomp_sdk::console::Console;
use kcomp_sdk::management;

extern "C" fn session(_arg: *mut ()) {
    Console::write(b"KaleidOS ksh\ntype 'help' for commands\nksh> ");
    let mut input = Input::new();
    loop {
        match Console::read_byte() {
            Ok(Some(byte)) => match input.feed(byte) {
                Event::None => {}
                Event::Echo(byte) => Console::write(&[byte]),
                Event::Erase => Console::write(b"\x08 \x08"),
                Event::Submit => {
                    Console::write(b"\n");
                    if crate::shell::execute(input.line()) {
                        break;
                    }
                    Console::write(b"ksh> ");
                }
                Event::Overflow => {
                    Console::write(b"\ninput rejected: line too long (max 128 bytes)\nksh> ")
                }
                Event::Cancel => Console::write(b"^C\nksh> "),
                Event::Exit => {
                    crate::shell::execute(b"exit");
                    break;
                }
            },
            Ok(None) => {
                if let Err(error) = management::yield_task() {
                    kcomp_sdk::klog!("ksh: yield failed: {error}");
                    break;
                }
            }
            Err(error) => {
                kcomp_sdk::klog!("ksh: console failed: {error}");
                break;
            }
        }
    }
    management::exit_task();
}

kcomp_sdk::kcomp_instance_create!(|_args, _out_state| {
    match management::start_task(session) {
        Ok(_) => 0,
        Err(error) => error.code(),
    }
});
kcomp_sdk::kcomp_instance_destroy!(|_state| { 0 });
