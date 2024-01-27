use core::fmt::Debug;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

use embedded_hal::timer::CountDown;

use crate::requests::{BorrowedRequest, Command};
use crate::{Interface, Request};

const PREAMBLE: [u8; 3] = [0x00, 0x00, 0xFF];
const POSTAMBLE: u8 = 0x00;
const ACK: [u8; 6] = [0x00, 0x00, 0xFF, 0x00, 0xFF, 0x00];

const HOST_TO_PN532: u8 = 0xD4;
const PN532_TO_HOST: u8 = 0xD5;

/// Pn532 Error
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum Error<E: Debug> {
    /// Could not parse ACK frame
    BadAck,
    /// Could not parse response frame
    BadResponseFrame,
    /// Received a syntax error frame
    Syntax,
    /// CRC for either the length or the data is wrong
    CrcError,
    /// The provided `response_len` was too low
    BufTooSmall,
    /// Did not receive an ACK frame in time
    TimeoutAck,
    /// Did not receive a response frame in time
    TimeoutResponse,
    /// Interface specific Error
    InterfaceError(E),
}

impl<E: Debug> From<E> for Error<E> {
    fn from(e: E) -> Self {
        Error::InterfaceError(e)
    }
}

/// Main struct of this crate
///
/// Provides blocking methods [`process`](Pn532::process) and [`process_async`](Pn532::process_async)
/// for sending requests and parsing responses.
///
/// Other methods can be used if fine-grain control is required.
///
/// # Note:
/// The `Pn532` uses an internal buffer for sending and receiving messages.
/// The size of the buffer is determined by the `N` type parameter which has a default value of `32`.
///
/// Choosing `N` too small will result in **panics**.
///
/// The following inequality should hold for all requests and responses:
/// ```text
/// N - 9 >= max(response_len, M)
/// ```
/// where
/// * `N` is the const generic type parameter of this struct.
/// * `response_len` is the largest number passed to
/// [`receive_response`](Pn532::receive_response), [`process`](Pn532::process) or [`process_async`](Pn532::process_async)
/// * `M` is the largest const generic type parameter of [`Request`] references passed to any sending methods of this struct
#[derive(Clone, Debug)]
pub struct Pn532<I, T, const N: usize = 32> {
    pub interface: I,
    pub timer: T,
    buf: [u8; N],
}

impl<I: Interface, T: CountDown, const N: usize> Pn532<I, T, N> {
    /// Send a request, wait for an ACK and then wait for a response.
    ///
    /// `response_len` is the largest expected length of the returned data.
    ///
    /// ```
    /// # use pn532::doc_test_helper::get_pn532;
    /// use pn532::Request;
    /// use pn532::IntoDuration; // trait for `ms()`, your HAL might have its own
    ///
    /// let mut pn532 = get_pn532();
    /// let result = pn532.process(&Request::GET_FIRMWARE_VERSION, 4, 50.ms());
    /// ```
    #[inline]
    pub fn process<const M: usize>(
        &mut self,
        request: &Request<M>,
        response_len: usize,
        timeout: T::Time,
    ) -> Result<&[u8], Error<I::Error>> {
        // codegen trampoline: https://github.com/rust-lang/rust/issues/77960
        self._process(request.borrow(), response_len, timeout)
    }
    fn _process(
        &mut self,
        request: BorrowedRequest<'_>,
        response_len: usize,
        timeout: T::Time,
    ) -> Result<&[u8], Error<I::Error>> {
        let sent_command = request.command;
        self.timer.start(timeout);
        self._send(request, &[])?;
        while self.interface.wait_ready()?.is_pending() {
            if self.timer.wait().is_ok() {
                return Err(Error::TimeoutAck);
            }
        }
        self.receive_ack()?;
        while self.interface.wait_ready()?.is_pending() {
            if self.timer.wait().is_ok() {
                return Err(Error::TimeoutAck);
            }
        }
        self.receive_response(sent_command, response_len)
    }


    #[inline]
    pub fn in_data_exchange< const M: usize>(
        & mut self,
        request: &Request<M>,
        response_len: usize,
        data: &[u8],
        timeout: T::Time,
    ) -> Result<&[u8], Error<I::Error>> {
        let sent_command = request.command;
        self.timer.start(timeout);

        self._send(request.borrow(), data)?;
        while self.interface.wait_ready()?.is_pending() {
            if self.timer.wait().is_ok() {
                return Err(Error::TimeoutAck);
            }
        }

        self.receive_ack()?;
        while self.interface.wait_ready()?.is_pending() {
            if self.timer.wait().is_ok() {
                return Err(Error::TimeoutAck);
            }
        }

        // self.receive_big_response(my_buf)
        self.receive_response(sent_command, response_len)
    }

    /// Send a request and wait for an ACK.
    ///
    /// ```
    /// # use pn532::doc_test_helper::get_pn532;
    /// use pn532::Request;
    /// use pn532::IntoDuration; // trait for `ms()`, your HAL might have its own
    ///
    /// let mut pn532 = get_pn532();
    /// pn532.process_no_response(&Request::INLIST_ONE_ISO_A_TARGET, 5.ms());
    /// ```
    #[inline]
    pub fn process_no_response<const M: usize>(
        &mut self,
        request: &Request<M>,
        timeout: T::Time,
    ) -> Result<(), Error<I::Error>> {
        // codegen trampoline: https://github.com/rust-lang/rust/issues/77960
        self._process_no_response(request.borrow(), timeout)
    }
    fn _process_no_response(
        &mut self,
        request: BorrowedRequest<'_>,
        timeout: T::Time,
    ) -> Result<(), Error<I::Error>> {
        self.timer.start(timeout);
        self._send(request, &[])?;
        while self.interface.wait_ready()?.is_pending() {
            if self.timer.wait().is_ok() {
                return Err(Error::TimeoutAck);
            }
        }
        self.receive_ack()
    }
}

impl<I: Interface, T, const N: usize> Pn532<I, T, N> {
    /// Create a Pn532 instance
    pub fn new(interface: I, timer: T) -> Self {
        Pn532 {
            interface,
            timer,
            buf: [0; N],
        }
    }

    /// Send a request.
    ///
    /// ```
    /// # use pn532::doc_test_helper::get_pn532;
    /// use pn532::Request;
    ///
    /// let mut pn532 = get_pn532();
    /// pn532.send(&Request::GET_FIRMWARE_VERSION);
    /// ```
    #[inline]
    pub fn send<const M: usize>(
        &mut self, request: &Request<M>,
        body: &[u8]
    ) -> Result<(), Error<I::Error>> {
        // codegen trampoline: https://github.com/rust-lang/rust/issues/77960
        self._send(request.borrow(), body)
    }
    fn _send(
        &mut self, request: BorrowedRequest<'_>,
        body: &[u8]
    ) -> Result<(), Error<I::Error>> {
        const fn to_checksum(sum: u8) -> u8 {
            (!sum).wrapping_add(1)
        }

        let command = request.command as u8;
        let hlen = request.data.len() as u8 + 1;
        let blen = body.len() as u8;

        let data_len = hlen + blen + 1;

        let mut i: usize = 0; self.buf[i] = PREAMBLE[0];
        i = i + 1; self.buf[i] = PREAMBLE[1];
        i = i + 1; self.buf[i] = PREAMBLE[2];
        i = i + 1; self.buf[i] = data_len;
        i = i + 1; self.buf[i] = to_checksum(data_len);
        i = i + 1; self.buf[i] = HOST_TO_PN532;
        i = i + 1; self.buf[i] = command;

        let mut data_sum = HOST_TO_PN532.wrapping_add(command); // sum(command + data + frame identifier)
        for &byte in request.data {
            if i <= 32 {
                i = i + 1; self.buf[i] = byte;
                data_sum = data_sum.wrapping_add(byte);
            } else {
                return Err(Error::CrcError);
            }
        }

        for &byte in body {
            if i <= 32 {
                i = i + 1; self.buf[i] = byte;
                data_sum = data_sum.wrapping_add(byte);
            } else {
                return Err(Error::CrcError);
            }
        }

        i = i + 1; self.buf[i] = to_checksum(data_sum);
        i = i + 1; self.buf[i] = POSTAMBLE;

        self.interface.write(&self.buf[..i])?;
        Ok(())
    }

    /// Receive an ACK frame.
    /// This should be done after [`send`](Pn532::send) was called and the interface was checked to be ready.
    ///
    /// ```
    /// # use pn532::doc_test_helper::get_pn532;
    /// use core::task::Poll;
    /// use pn532::{Interface, Request};
    ///
    /// let mut pn532 = get_pn532();
    /// pn532.send(&Request::GET_FIRMWARE_VERSION);
    /// // do something else
    /// if let Poll::Ready(Ok(_)) = pn532.interface.wait_ready() {
    ///     pn532.receive_ack();
    /// }
    /// ```
    pub fn receive_ack(&mut self) -> Result<(), Error<I::Error>> {
        let mut ack_buf = [0; 6];
        self.interface.read(&mut ack_buf)?;
        if ack_buf != ACK {
            Err(Error::BadAck)
        } else {
            Ok(())
        }
    }

    /// Receive a response frame.
    /// This should be done after [`send`](Pn532::send) and [`receive_ack`](Pn532::receive_ack) was called and
    /// the interface was checked to be ready.
    ///
    /// `response_len` is the largest expected length of the returned data.
    ///
    /// ```
    /// # use pn532::doc_test_helper::get_pn532;
    /// use core::task::Poll;
    /// use pn532::{Interface, Request};
    ///
    /// let mut pn532 = get_pn532();
    /// pn532.send(&Request::GET_FIRMWARE_VERSION);
    /// // do something else
    /// if let Poll::Ready(Ok(_)) = pn532.interface.wait_ready() {
    ///     pn532.receive_ack();
    /// }
    /// // do something else
    /// if let Poll::Ready(Ok(_)) = pn532.interface.wait_ready() {
    ///     let result = pn532.receive_response(Request::GET_FIRMWARE_VERSION.command, 4);
    /// }
    /// ```
    pub fn receive_response(
        &mut self,
        sent_command: Command,
        response_len: usize,
    ) -> Result<&[u8], Error<I::Error>> {
        let response_buf = &mut self.buf[..response_len + 9];
        response_buf.fill(0); // zero out buf
        self.interface.read(response_buf)?;
        let expected_response_command = sent_command as u8 + 1;
        parse_response(response_buf, expected_response_command)
    }

    pub fn receive_big_response<'a>(
        &'a mut self,
        my_buff: &'a mut [u8],
    ) -> Result<(), Error<I::Error>> {
        self.interface.read( my_buff)?;

        Ok(())
    }

    /// Send an ACK frame to force the PN532 to abort the current process.
    /// In that case, the PN532 discontinues the last processing and does not answer anything
    /// to the host controller.
    /// Then, the PN532 starts again waiting for a new command.
    pub fn abort(&mut self) -> Result<(), Error<I::Error>> {
        self.interface.write(&ACK)?;
        Ok(())
    }
}

impl<I: Interface, const N: usize> Pn532<I, (), N> {
    /// Create a Pn532 instance without a timer
    pub fn new_async(interface: I) -> Self {
        Pn532 {
            interface,
            timer: (),
            buf: [0; N],
        }
    }

    /// Send a request, wait for an ACK and then wait for a response.
    ///
    /// `response_len` is the largest expected length of the returned data.
    ///
    /// ```
    /// # use pn532::doc_test_helper::get_async_pn532;
    /// use pn532::Request;
    ///
    /// let mut pn532 = get_async_pn532();
    /// let future = pn532.process_async(&Request::GET_FIRMWARE_VERSION, 4);
    /// ```
    #[inline]
    pub async fn process_async<const M: usize>(
        &mut self,
        request: &Request<M>,
        response_len: usize,
    ) -> Result<&[u8], Error<I::Error>> {
        // codegen trampoline: https://github.com/rust-lang/rust/issues/77960
        self._process_async(request.borrow(), response_len).await
    }
    async fn _process_async(
        &mut self,
        request: BorrowedRequest<'_>,
        response_len: usize,
    ) -> Result<&[u8], Error<I::Error>> {
        let sent_command = request.command;
        self._send(request, &[])?;
        self.wait_ready_future().await?;
        self.receive_ack()?;
        self.wait_ready_future().await?;
        self.receive_response(sent_command, response_len)
    }

    /// Send a request and wait for an ACK.
    ///
    /// ```
    /// # use pn532::doc_test_helper::get_async_pn532;
    /// use pn532::Request;
    ///
    /// let mut pn532 = get_async_pn532();
    /// let future = pn532.process_no_response_async(&Request::INLIST_ONE_ISO_A_TARGET);
    #[inline]
    pub async fn process_no_response_async<const M: usize>(
        &mut self,
        request: &Request<M>,
    ) -> Result<(), Error<I::Error>> {
        // codegen trampoline: https://github.com/rust-lang/rust/issues/77960
        self._process_no_response_async(request.borrow()).await
    }
    async fn _process_no_response_async(
        &mut self,
        request: BorrowedRequest<'_>,
    ) -> Result<(), Error<I::Error>> {
        self._send(request, &[])?;
        self.wait_ready_future().await?;
        self.receive_ack()?;
        Ok(())
    }

    fn wait_ready_future(&mut self) -> WaitReadyFuture<I> {
        WaitReadyFuture {
            interface: &mut self.interface,
        }
    }
}

fn parse_response<E: Debug>(
    response_buf: &[u8],
    expected_response_command: u8,
) -> Result<&[u8], Error<E>> {
    if response_buf[0..3] != PREAMBLE {
        return Err(Error::BadResponseFrame);
    }
    // Check length & length checksum
    let frame_len = response_buf[3];
    if (frame_len.wrapping_add(response_buf[4])) != 0 {
        return Err(Error::CrcError);
    }
    if frame_len == 0 {
        return Err(Error::BadResponseFrame);
    }
    if frame_len == 1 {
        // 6.2.1.5 Error frame
        return Err(Error::Syntax);
    }
    match response_buf.get(5 + frame_len as usize + 1) {
        None => {
            return Err(Error::BufTooSmall);
        }
        Some(&POSTAMBLE) => {}
        Some(_) => {
            return Err(Error::BadResponseFrame);
        }
    }

    if response_buf[5] != PN532_TO_HOST || response_buf[6] != expected_response_command {
        return Err(Error::BadResponseFrame);
    }
    // Check frame checksum value matches bytes
    let checksum = response_buf[5..5 + frame_len as usize + 1]
        .iter()
        .fold(0u8, |s, &b| s.wrapping_add(b));
    if checksum != 0 {
        return Err(Error::CrcError);
    }
    // Adjust response buf and return it
    Ok(&response_buf[7..5 + frame_len as usize])

    // Ok(&response_buf[..])
}

struct WaitReadyFuture<'a, I> {
    interface: &'a mut I,
}

impl<'a, I: Interface> Future for WaitReadyFuture<'a, I> {
    type Output = Result<(), I::Error>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let poll = self.interface.wait_ready();
        if poll.is_pending() {
            // tell the executor to poll this future again
            cx.waker().clone().wake();
        }
        poll
    }
}