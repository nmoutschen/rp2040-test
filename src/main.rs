//! # Pico USB Serial (with Interrupts) Example
//!
//! Creates a USB Serial device on a Pico board, with the USB driver running in
//! the USB interrupt.
//!
//! This will create a USB Serial device echoing anything it receives. Incoming
//! ASCII characters are converted to upercase, so you can tell it is working
//! and not just local-echo!
//!
//! See the `Cargo.toml` file for Copyright and licence details.

#![no_std]
#![no_main]

// The macro for our start-up function
use cortex_m_rt::entry;

use display_interface_spi::SPIInterface;
use embedded_graphics::{
    draw_target::DrawTarget,
    image::{Image, ImageRaw, ImageRawLE},
    mono_font::{iso_8859_15::FONT_6X10, MonoTextStyleBuilder},
    pixelcolor::{Rgb565, RgbColor},
    prelude::*,
};
// The macro for marking our interrupt functions
use rp2040_test::hal::pac::interrupt;
use rp2040_test::terminal::{Terminal, TerminalBuilder};

// GPIO traits
use embedded_hal::digital::v2::OutputPin;

// Time handling traits
use embedded_time::rate::*;

// Ensure we halt the program on panic (if we don't mention this crate it won't
// be linked)
use panic_halt as _;

// Pull in any important traits
// use pico::hal::prelude::*;
use rp2040_test::hal::prelude::*;

// A shorter alias for the Peripheral Access Crate, which provides low-level
// register access
// use pico::hal::pac;
use rp2040_test::hal::pac;

// A shorter alias for the Hardware Abstraction Layer, which provides
// higher-level drivers.
// use pico::hal;
use rp2040_test::hal;

// USB Device support
use usb_device::{class_prelude::*, prelude::*};

// USB Communications Class Device support
use usbd_serial::SerialPort;

/// The USB Device Driver (shared with the interrupt).
static mut USB_DEVICE: Option<UsbDevice<hal::usb::UsbBus>> = None;

/// The USB Bus Driver (shared with the interrupt).
static mut USB_BUS: Option<UsbBusAllocator<hal::usb::UsbBus>> = None;

/// The USB Serial Device Driver (shared with the interrupt).
static mut USB_SERIAL: Option<SerialPort<hal::usb::UsbBus>> = None;

// static mut SCREEN: Option<
//     st7789::ST7789<
//         SPIInterface<
//             hal::spi::Spi<hal::spi::Enabled, pac::SPI0, 8>,
//             hal::gpio::pin::Pin<hal::gpio::pin::bank0::Gpio16, hal::gpio::pin::PushPullOutput>,
//             hal::gpio::pin::Pin<hal::gpio::pin::bank0::Gpio17, hal::gpio::pin::PushPullOutput>,
//         >,
//         rp2040_test::DummyPin,
//     >,
// > = None;
// static mut SCREEN_POS: Option<Point> = None;
static mut TERMINAL: Option<
    Terminal<
        Rgb565,
        st7789::ST7789<
            SPIInterface<
                hal::spi::Spi<hal::spi::Enabled, pac::SPI0, 8>,
                hal::gpio::pin::Pin<hal::gpio::pin::bank0::Gpio16, hal::gpio::pin::PushPullOutput>,
                hal::gpio::pin::Pin<hal::gpio::pin::bank0::Gpio17, hal::gpio::pin::PushPullOutput>,
            >,
            rp2040_test::DummyPin,
        >,
    >,
> = None;

static FERRIS: &[u8] = include_bytes!("../ferris.raw");

/// Entry point to our bare-metal application.
///
/// The `#[entry]` macro ensures the Cortex-M start-up code calls this function
/// as soon as all global variables are initialised.
///
/// The function configures the RP2040 peripherals, then blinks the LED in an
/// infinite loop.
#[entry]
fn main() -> ! {
    // Grab our singleton objects
    let mut pac = pac::Peripherals::take().unwrap();
    let core = pac::CorePeripherals::take().unwrap();

    // Set up the watchdog driver - needed by the clock setup code
    let mut watchdog = hal::watchdog::Watchdog::new(pac.WATCHDOG);

    // Configure the clocks
    //
    // The default is to generate a 125 MHz system clock
    let clocks = hal::clocks::init_clocks_and_plls(
        rp2040_test::XOSC_CRYSTAL_FREQ,
        pac.XOSC,
        pac.CLOCKS,
        pac.PLL_SYS,
        pac.PLL_USB,
        &mut pac.RESETS,
        &mut watchdog,
    )
    .ok()
    .unwrap();

    // Set up the USB driver
    let usb_bus = UsbBusAllocator::new(hal::usb::UsbBus::new(
        pac.USBCTRL_REGS,
        pac.USBCTRL_DPRAM,
        clocks.usb_clock,
        true,
        &mut pac.RESETS,
    ));
    unsafe {
        // Note (safety): This is safe as interrupts haven't been started yet
        USB_BUS = Some(usb_bus);
    }

    // Grab a reference to the USB Bus allocator. We are promising to the
    // compiler not to take mutable access to this global variable whilst this
    // reference exists!
    let bus_ref = unsafe { USB_BUS.as_ref().unwrap() };

    // Set up the USB Communications Class Device driver
    let serial = SerialPort::new(bus_ref);
    unsafe {
        USB_SERIAL = Some(serial);
    }

    // Create a USB device with a fake VID and PID
    let usb_dev = UsbDeviceBuilder::new(bus_ref, UsbVidPid(0x16c0, 0x27dd))
        .manufacturer("Fake company")
        .product("Serial port")
        .serial_number("TEST")
        .device_class(2) // from: https://www.usb.org/defined-class-codes
        .build();
    unsafe {
        // Note (safety): This is safe as interrupts haven't been started yet
        USB_DEVICE = Some(usb_dev);
    }

    // The delay object lets us wait for specified amounts of time (in
    // milliseconds)
    let mut delay = cortex_m::delay::Delay::new(core.SYST, clocks.system_clock.freq().integer());

    // The single-cycle I/O block controls our GPIO pins
    let sio = hal::sio::Sio::new(pac.SIO);

    // Set the pins up according to their function on this particular board
    let pins = rp2040_test::Pins::new(
        pac.IO_BANK0,
        pac.PADS_BANK0,
        sio.gpio_bank0,
        &mut pac.RESETS,
    );

    // Configure the display
    let dc = pins.lcd_dc.into_push_pull_output();
    let cs = pins.lcd_cs.into_push_pull_output();
    let _spi_sclk = pins.spi_sclk.into_mode::<hal::gpio::pin::FunctionSpi>();
    let _spi_mosi = pins.spi_mosi.into_mode::<hal::gpio::pin::FunctionSpi>();

    let spi_screen = hal::spi::Spi::<_, _, 8>::new(pac.SPI0).init(
        &mut pac.RESETS,
        125_000_000u32.Hz(),
        16_000_000u32.Hz(),
        &embedded_hal::spi::MODE_0,
    );
    let spii_screen = SPIInterface::new(spi_screen, dc, cs);
    let mut screen = st7789::ST7789::new(spii_screen, rp2040_test::DummyPin, 240, 135);
    screen.init(&mut delay).unwrap();
    screen
        .set_orientation(st7789::Orientation::LandscapeSwapped)
        .unwrap();
    screen.clear(Rgb565::BLACK).unwrap();

    // Draw ferris
    let ferris: ImageRawLE<Rgb565> = ImageRaw::new(FERRIS, 64);
    let ferris_img = Image::new(&ferris, Point::new(40, 50));
    ferris_img.draw(&mut screen).unwrap();

    // Setup the terminal
    let mut terminal = TerminalBuilder::new(
        screen,
        MonoTextStyleBuilder::new()
            .font(&FONT_6X10)
            .text_color(Rgb565::RED)
            .build(),
    )
    .with_offset(Point::new(40, 60))
    .build();
    terminal.write(b"Hello, world!\n");

    unsafe {
        // SCREEN = Some(screen);
        // SCREEN_POS = Some(Point::new(40, 100));
        TERMINAL = Some(terminal);
    }

    // Enable the USB interrupt
    unsafe {
        pac::NVIC::unmask(hal::pac::Interrupt::USBCTRL_IRQ);
    };

    // No more USB code after this point in main! We can do anything we want in
    // here since USB is handled in the interrupt - let's blink an LED!

    // Set the LED to be an output
    let mut led_pin = pins.led.into_push_pull_output();

    // Blink the LED at 1 Hz
    loop {
        led_pin.set_high().unwrap();
        delay.delay_ms(500);
        led_pin.set_low().unwrap();
        delay.delay_ms(500);
    }
}

/// This function is called whenever the USB Hardware generates an Interrupt
/// Request.
///
/// We do all our USB work under interrupt, so the main thread can continue on
/// knowing nothing about USB.
#[allow(non_snake_case)]
#[interrupt]
unsafe fn USBCTRL_IRQ() {
    use core::sync::atomic::{AtomicBool, Ordering};

    /// Note whether we've already printed the "hello" message.
    static SAID_HELLO: AtomicBool = AtomicBool::new(false);

    // Grab the global objects. This is OK as we only access them under interrupt.
    let usb_dev = USB_DEVICE.as_mut().unwrap();
    let serial = USB_SERIAL.as_mut().unwrap();

    // Say hello exactly once on start-up
    if !SAID_HELLO.load(Ordering::Relaxed) {
        SAID_HELLO.store(true, Ordering::Relaxed);
        let _ = serial.write(b"Hello, World!\r\n");
    }

    // Poll the USB driver with all of our supported USB Classes
    if usb_dev.poll(&mut [serial]) {
        let mut buf = [0u8; 64];
        match serial.read(&mut buf) {
            Err(_e) => {
                // Do nothing
            }
            Ok(0) => {
                // Do nothing
            }
            Ok(count) => {
                // Write to the screen
                let terminal = TERMINAL.as_mut().unwrap();
                terminal.write(&buf[0..count]);

                // Convert to lower case
                buf.iter_mut().take(count).for_each(|b| {
                    b.make_ascii_lowercase();
                });

                // Send back to the host
                let mut wr_ptr = &buf[..count];
                while !wr_ptr.is_empty() {
                    let _ = serial.write(wr_ptr).map(|len| {
                        wr_ptr = &wr_ptr[len..];
                    });
                }
            }
        }
    }
}

// End of file
