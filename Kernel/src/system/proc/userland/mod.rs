// Copyright (c) ChefKiss Inc 2021-2023. Licensed under the Thou Shalt Not Profit License version 1.0. See LICENSE for details.

use tungstenkit::syscall::{KernelMessage, Message, SystemCall};

use crate::system::{gdt::PrivilegeLevel, RegisterState};

mod handlers;
pub mod page_table;

unsafe extern "C" fn irq_handler(state: &mut RegisterState) {
    let irq = (state.int_num - 0x20) as u8;
    crate::acpi::ioapic::set_irq_mask(irq, true);
    let mut scheduler = (*crate::system::state::SYS_STATE.get())
        .scheduler
        .as_ref()
        .unwrap()
        .lock();
    let pid = scheduler.irq_handlers.get(&irq).cloned().unwrap();
    let s = postcard::to_allocvec(&KernelMessage::IRQFired(irq))
        .unwrap()
        .leak();

    let virt = scheduler
        .processes
        .get_mut(&pid)
        .unwrap()
        .track_kernelside_alloc(s.as_ptr() as _, s.len() as _);

    let msg = Message::new(
        scheduler.msg_id_gen.next(),
        0,
        core::slice::from_raw_parts(virt as *const _, s.len() as _),
    );
    scheduler.message_sources.insert(msg.id, 0);
    let process = scheduler.processes.get_mut(&pid).unwrap();
    process.track_msg(msg.id, virt);

    let tids = process.tids.clone();
    let idle = scheduler.current_tid.is_none();
    for tid in tids.into_iter() {
        let thread = scheduler.threads.get_mut(&tid).unwrap();
        if thread.state == super::ThreadState::Suspended {
            thread.state = super::ThreadState::Inactive;
            if idle {
                drop(scheduler);
                super::scheduler::schedule(state);
                state.rdi = msg.id;
                state.rsi = msg.pid;
                state.rdx = msg.data.as_ptr() as _;
                state.rcx = msg.data.len() as _;
                return;
            }
            break;
        }
    }

    let process = scheduler.processes.get_mut(&pid).unwrap();
    process.messages.push_front(msg);
}

unsafe extern "C" fn syscall_handler(state: &mut RegisterState) {
    let sys_state = &mut *crate::system::state::SYS_STATE.get();
    let mut scheduler = sys_state.scheduler.as_ref().unwrap().lock();

    let Ok(v) = SystemCall::try_from(state.rdi) else {
        todo!();
        // return;
    };

    match v {
        SystemCall::KPrint => handlers::kprint(state),
        SystemCall::ReceiveMessage => handlers::message::receive(&mut scheduler, state),
        SystemCall::SendMessage => handlers::message::send(&mut scheduler, state),
        SystemCall::Quit => {
            handlers::thread_teardown(&mut scheduler);
            drop(scheduler);
            super::scheduler::schedule(state);
            return;
        }
        SystemCall::Yield => {
            drop(scheduler);
            super::scheduler::schedule(state);
            return;
        }
        SystemCall::RegisterProvider => handlers::provider::register(&mut scheduler, state),
        SystemCall::GetProvidingProcess => handlers::provider::get(&mut scheduler, state),
        SystemCall::PortIn => handlers::port::port_in(state),
        SystemCall::PortOut => handlers::port::port_out(state),
        SystemCall::RegisterIRQHandler => {
            let irq = state.rsi as u8;
            if irq > 0xDF {
                todo!()
            }
            let pid = scheduler.current_pid.unwrap();
            if scheduler.irq_handlers.try_insert(irq, pid).is_err() {
                todo!()
            }

            crate::acpi::ioapic::wire_legacy_irq(irq, false);
            crate::intrs::idt::set_handler(
                irq + 0x20,
                1,
                PrivilegeLevel::Supervisor,
                irq_handler,
                true,
                true,
            );
        }
        SystemCall::Allocate => handlers::alloc::alloc(&mut scheduler, state),
        SystemCall::Free => handlers::alloc::free(&mut scheduler, state),
        SystemCall::AckMessage => handlers::message::ack(&mut scheduler, state),
        SystemCall::GetDTEntryInfo => handlers::device_tree::get_entry_info(&mut scheduler, state),
    }

    if scheduler.current_thread_mut().unwrap().state == super::ThreadState::Suspended {
        drop(scheduler);
        super::scheduler::schedule(state);
    }
}

pub fn setup() {
    crate::intrs::idt::set_handler(249, 1, PrivilegeLevel::User, syscall_handler, false, true);
}
