#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]
use embassy_executor::Spawner;
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, channel::Channel};
use embassy_time::{Duration, Timer, with_timeout};
use embedded_io_async::{Read, Write};
use esp_hal::clock::CpuClock;
use esp_hal::gpio::Level;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::uart::{Config as UartConfig, Uart};
use esp_hal::{
    rmt::{PulseCode, Rmt, TxChannelConfig, TxChannelCreator},
    time::Rate,
};
use esp32_remote_gate::config::{ALLOWED_NUMBERS, GATE_CODE};
use heapless::Vec;
use log::{error, info};
#[derive(Clone, Copy, Debug)]
enum RfCommand {
    TriggerCode1,
}

static RF_CHANNEL: Channel<CriticalSectionRawMutex, RfCommand, 4> = Channel::new();

#[panic_handler]
fn panic(panic_info: &core::panic::PanicInfo) -> ! {
    error!("{}", panic_info);
    loop {}
}

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    // generator version: 2.0.0
    // generator parameters: -o esp32c3 -o esp32c3-mini-1 -o unstable-hal -o embassy -o log -o zed

    esp_println::logger::init_logger_from_env();

    info!("Embassy initialized!");

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    esp_alloc::heap_allocator!(size: 72 * 1024);
    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    let rmt = Rmt::new(peripherals.RMT, Rate::from_mhz(1))
        .unwrap()
        .into_async();
    let tx_config = TxChannelConfig::default()
        .with_clk_divider(1)
        .with_idle_output_level(Level::Low)
        .with_idle_output(true)
        .with_carrier_modulation(false);

    let mut channel = rmt
        .channel0
        .configure_tx(&tx_config)
        .unwrap()
        .with_pin(peripherals.GPIO2);

    let mut pulses: Vec<PulseCode, 424> = GATE_CODE
        .chunks(2)
        .map(|pair| {
            PulseCode::new(
                Level::High,
                pair[0] as u16,
                Level::Low,
                (pair[1] * -1) as u16,
            )
        })
        .collect();
    pulses.push(PulseCode::end_marker());

    let uart_config = UartConfig::default().with_baudrate(9600);
    let uart1 = Uart::new(peripherals.UART1, uart_config)
        .unwrap()
        .with_tx(peripherals.GPIO6)
        .with_rx(peripherals.GPIO7)
        .into_async();

    spawner.spawn(sim800l_task(uart1).unwrap());
    loop {
        let cmd = RF_CHANNEL.receive().await;
        info!("RF command: {:?}", cmd);
        match cmd {
            RfCommand::TriggerCode1 => {
                info!("opening garage door");
                for _ in 0..3 {
                    info!("Sending pulses...");
                    let t0 = embassy_time::Instant::now();
                    channel.transmit(&pulses).await.unwrap();
                    info!("tx took {} ms", t0.elapsed().as_millis());
                    info!("Done!");
                }
            }
        }
    }
}

type Modem = Uart<'static, esp_hal::Async>;
async fn at(uart: &mut Modem, cmd: &str, idle_ms: u64) -> heapless::Vec<u8, 256> {
    let _ = uart.write_all(cmd.as_bytes()).await;
    let _ = uart.write_all(b"\r\n").await;

    let mut buf = [0u8; 256];
    let mut len = 0;
    while len < buf.len() {
        match with_timeout(
            Duration::from_millis(idle_ms),
            Read::read(&mut *uart, &mut buf[len..]),
        )
        .await
        {
            Ok(Ok(n)) if n > 0 => len += n,
            _ => break,
        }
    }

    match core::str::from_utf8(&buf[..len]) {
        Ok(s) => info!("{} -> {}", cmd, s.trim()),
        Err(_) => info!("{} -> (non-utf8) {:?}", cmd, &buf[..len]),
    }
    heapless::Vec::from_slice(&buf[..len]).unwrap()
}

fn is_accepted_number(number: &str) -> bool {
    ALLOWED_NUMBERS.iter().any(|(_, n)| {
        info!("checking number: {} against: {}", number, n);
        number.contains(n)
    })
}

#[embassy_executor::task]
async fn sim800l_task(mut uart: Modem) {
    Timer::after(Duration::from_secs(5)).await;
    info!("SIM800L task started");

    // Autobaud sync: send AT a few times
    for _ in 0..3 {
        at(&mut uart, "AT", 300).await;
    }

    // Diagnostics
    at(&mut uart, "ATI", 300).await; // module/firmware info
    at(&mut uart, "AT+CBC", 300).await; // supply voltage in mV, useful to check power
    at(&mut uart, "AT+CPIN?", 500).await; // READY = SIM ok
    at(&mut uart, "AT+CSQ", 300).await; // signal: 10-31 good, 99 = none
    at(&mut uart, "AT+COPS?", 2000).await; // operator

    // Wait for network registration (+CREG: 0,1 or 0,5)
    let mut registered = false;
    for i in 0..30 {
        info!("CREG poll {}", i);
        let reply = at(&mut uart, "AT+CREG?", 500).await;
        let s = core::str::from_utf8(&reply).unwrap_or("");
        if s.contains("+CREG: 0,1") || s.contains("+CREG: 0,5") {
            registered = true;
            break;
        }
        Timer::after(Duration::from_secs(2)).await;
    }

    if registered {
        info!("Network registered");
    } else {
        info!("Network not registered");
        return;
    }

    // Init
    at(&mut uart, "ATE0", 300).await;
    at(&mut uart, "AT+CMGF=1", 300).await;
    at(&mut uart, "AT+CNMI=2,2,0,0,0", 300).await;
    at(&mut uart, "AT+CLIP=1", 300).await;
    info!("SIM800L initialized");

    let mut buf = [0u8; 256];
    loop {
        match Read::read(&mut uart, &mut buf).await {
            Ok(n) if n > 0 => {
                if let Ok(text) = core::str::from_utf8(&buf[..n]) {
                    info!("before");
                    info!("Received: {}", text);
                    let (command, body) = text.trim().split_once("\r\n").unwrap_or((text, ""));
                    let open_gate = match command {
                        s if s.starts_with("+CMT") => {
                            is_accepted_number(s) && body.contains("OPEN_GATE")
                        }
                        // starts with +CLIP
                        s if s.starts_with("RING") => {
                            at(&mut uart, "ATH", 300).await;
                            is_accepted_number(body)
                        }
                        _ => false,
                    };
                    if open_gate {
                        let _ = RF_CHANNEL.try_send(RfCommand::TriggerCode1);
                    }
                }
            }
            Ok(_) => {}
            Err(e) => info!("UART error: {:?}", e),
        }
    }
}
