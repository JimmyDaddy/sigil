//! Unix direct-child ownership with a single non-consuming reaper.
//!
//! This module deliberately produces only bounded native cleanup facts. It keeps the direct
//! child as a zombie with `waitid(..., WNOWAIT)` while cleanup uses the original process-group
//! number, then consumes that exact child once. A caller that needs contained-tree proof must use
//! a backend with a closed member set instead of reinterpreting this receipt as one.

use std::{
    collections::BTreeMap,
    io,
    process::{Child, ChildStderr, ChildStdout, ExitStatus},
    thread,
    time::{Duration, Instant},
};

use nix::{
    errno::Errno,
    sys::signal::{self, Signal},
    unistd::Pid,
};

use crate::{ProcessIdentityObservationErrorV1, ProcessIdentityV1, observe_process_identity};

const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// A direct-child terminal observation that intentionally has not reaped the child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NonConsumingChildTerminalObservationV1 {
    StillRunning,
    Terminal,
}

/// Raw bounded-cleanup facts for a native Unix attempt.
///
/// This is not a contained-tree receipt. It says that one directly owned child was reaped, the
/// original process group was absent after reaping, and every explicitly registered member was
/// terminal under its exact birth identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoundedNativeCleanupReceiptV1 {
    direct_child_process_id: u32,
    registered_member_count: usize,
}

impl BoundedNativeCleanupReceiptV1 {
    #[must_use]
    pub const fn direct_child_process_id(&self) -> u32 {
        self.direct_child_process_id
    }

    #[must_use]
    pub const fn registered_member_count(&self) -> usize {
        self.registered_member_count
    }
}

/// The common lifecycle interface that PTY ownership must implement without calling
/// `try_wait` or otherwise consuming the direct child before bounded cleanup is complete.
pub trait OwnedChildLifecycleV1 {
    /// Reads terminal state without consuming the direct child status.
    fn observe_terminal_without_reap(
        &mut self,
    ) -> Result<NonConsumingChildTerminalObservationV1, OwnedChildReaperErrorV1>;

    /// Runs bounded cleanup and consumes the one direct-child status exactly once.
    fn cleanup_and_reap(
        &mut self,
        deadline: Instant,
        terminate_grace: Duration,
    ) -> Result<BoundedNativeCleanupReceiptV1, OwnedChildReaperErrorV1>;
}

/// A non-cloneable Unix direct-child owner. Owning the `Child` prevents a second local reaper
/// from racing `waitid(..., WNOWAIT)` with `wait`/`try_wait`.
pub struct UnixOwnedChildReaperV1 {
    child: Child,
    direct_child_process_id: u32,
    // A direct child that is already retained as a zombie by the kernel can no longer produce a
    // live birth observation, but `WNOWAIT` still pins its PID and lets us clean the original
    // group before the sole reap. Live leaders must continue to have an exact birth identity.
    direct_child_identity: Option<ProcessIdentityV1>,
    process_group_id: i32,
    known_members: BTreeMap<u32, ProcessIdentityV1>,
    reaped: bool,
    terminal_exit_status: Option<ExitStatus>,
}

impl std::fmt::Debug for UnixOwnedChildReaperV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UnixOwnedChildReaperV1")
            .field("direct_child_process_id", &self.direct_child_process_id())
            .field("registered_member_count", &self.known_members.len())
            .field("reaped", &self.reaped)
            .finish()
    }
}

impl UnixOwnedChildReaperV1 {
    /// Takes exclusive ownership of a just-spawned direct child configured with
    /// [`crate::configure_process_tree`].
    ///
    /// # Errors
    ///
    /// Returns an error if the direct child cannot be observed live with an OS birth identity or
    /// if it is not currently the leader of the owned process group. This compatibility helper
    /// terminates and reaps the direct child before returning its error. Callers that must retain
    /// the handle and record their own recovery debt should use [`Self::adopt_with_child`].
    pub fn adopt(child: Child) -> Result<Self, OwnedChildReaperErrorV1> {
        match Self::adopt_with_child(child) {
            Ok(reaper) => Ok(reaper),
            Err((mut child, error)) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(error)
            }
        }
    }

    /// Takes ownership of a just-spawned child while returning that same handle on an adoption
    /// failure. This keeps caller-owned cleanup and durable claim settlement explicit.
    ///
    /// # Errors
    ///
    /// Returns the original child alongside the exact ownership-proof error when adoption cannot
    /// establish a safe owned process group.
    pub fn adopt_with_child(child: Child) -> Result<Self, (Child, OwnedChildReaperErrorV1)> {
        let process_id = child.id();
        let (direct_child_identity, process_group_id) = match Self::adopt_metadata(process_id) {
            Ok(metadata) => metadata,
            Err(error) => return Err((child, error)),
        };
        Ok(Self {
            child,
            direct_child_process_id: process_id,
            direct_child_identity,
            process_group_id,
            known_members: BTreeMap::new(),
            reaped: false,
            terminal_exit_status: None,
        })
    }

    fn adopt_metadata(
        process_id: u32,
    ) -> Result<(Option<ProcessIdentityV1>, i32), OwnedChildReaperErrorV1> {
        let expected_process_group_id = i32::try_from(process_id)
            .map_err(|_| OwnedChildReaperErrorV1::InvalidProcessId(process_id))?;
        let terminal = observe_child_terminal_without_reap(process_id)?;
        let direct_child_identity = match observe_process_identity(process_id) {
            Ok(identity) => Some(identity),
            Err(_error) if terminal == NonConsumingChildTerminalObservationV1::Terminal => None,
            Err(error) => return Err(OwnedChildReaperErrorV1::DirectChildObservation(error)),
        };
        let process_group_id = match current_process_group(process_id) {
            Ok(process_group_id) if process_group_id == expected_process_group_id => {
                process_group_id
            }
            Ok(process_group_id) => {
                return Err(OwnedChildReaperErrorV1::ProcessGroupOwnershipUnproven {
                    expected: process_id,
                    observed: process_group_id,
                });
            }
            // A terminal direct child with no surviving members has already released its group.
            // It is still safe to reap the exact direct-child handle; cleanup becomes a bounded
            // no-op rather than manufacturing an ownership failure for a normal fast command.
            Err(Errno::ESRCH) if terminal == NonConsumingChildTerminalObservationV1::Terminal => {
                expected_process_group_id
            }
            Err(error) => return Err(OwnedChildReaperErrorV1::ProcessGroupObservation(error)),
        };
        Ok((direct_child_identity, process_group_id))
    }

    #[must_use]
    pub const fn direct_child_process_id(&self) -> u32 {
        self.direct_child_process_id
    }

    /// Takes the owned stdout pipe without exposing the direct child wait handle.
    #[must_use]
    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.stdout.take()
    }

    /// Takes the owned stderr pipe without exposing the direct child wait handle.
    #[must_use]
    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.child.stderr.take()
    }

    /// Returns the exact direct-child status only after bounded cleanup has consumed it.
    #[must_use]
    pub fn direct_child_exit_status(&self) -> Option<&ExitStatus> {
        self.terminal_exit_status.as_ref()
    }

    /// Records an attempt-ledger member that was actually observed with an OS birth identity.
    ///
    /// Membership authority stays with the caller's sandbox ledger. This reaper only preserves
    /// the supplied exact identity so a `setsid`/PGID escape cannot later be forgotten or killed
    /// by a reused numeric PID.
    pub fn register_known_member(
        &mut self,
        identity: ProcessIdentityV1,
    ) -> Result<(), OwnedChildReaperErrorV1> {
        let process_id = identity.process_id();
        if process_id == self.direct_child_process_id() {
            return Err(OwnedChildReaperErrorV1::DirectChildCannotBeRegisteredAsMember);
        }
        if self.known_members.insert(process_id, identity).is_some() {
            return Err(OwnedChildReaperErrorV1::DuplicateKnownMember(process_id));
        }
        Ok(())
    }

    fn ensure_not_reaped(&self) -> Result<(), OwnedChildReaperErrorV1> {
        if self.reaped {
            return Err(OwnedChildReaperErrorV1::AlreadyReaped);
        }
        Ok(())
    }

    fn ensure_direct_child_identity_or_terminal(
        &mut self,
    ) -> Result<NonConsumingChildTerminalObservationV1, OwnedChildReaperErrorV1> {
        let terminal = self.observe_terminal_without_reap()?;
        if terminal == NonConsumingChildTerminalObservationV1::Terminal {
            return Ok(terminal);
        }
        match (
            &self.direct_child_identity,
            observe_process_identity(self.direct_child_process_id()),
        ) {
            (Some(expected), Ok(identity)) if identity == *expected => Ok(terminal),
            (Some(_), Ok(_)) | (None, Ok(_)) => {
                Err(OwnedChildReaperErrorV1::DirectChildBirthIdentityMismatch)
            }
            (_, Err(error)) => Err(OwnedChildReaperErrorV1::DirectChildObservation(error)),
        }
    }

    fn ensure_original_process_group(
        &self,
        terminal: NonConsumingChildTerminalObservationV1,
    ) -> Result<bool, OwnedChildReaperErrorV1> {
        match current_process_group(self.direct_child_process_id()) {
            Ok(observed) if observed == self.process_group_id => Ok(true),
            Ok(observed) => Err(OwnedChildReaperErrorV1::ProcessGroupDrift {
                expected: self.process_group_id,
                observed,
            }),
            // A non-consuming terminal observation still pins the direct child's PID. macOS can
            // report ESRCH for `getpgid` of that zombie even while same-group descendants remain,
            // so retain the original PGID and let `killpg` distinguish a live group from an exact
            // no-op before the sole reap makes numeric reuse possible.
            Err(Errno::ESRCH) if terminal == NonConsumingChildTerminalObservationV1::Terminal => {
                Ok(true)
            }
            Err(error) => Err(OwnedChildReaperErrorV1::ProcessGroupObservation(error)),
        }
    }

    fn signal_owned_group(
        &self,
        terminal: NonConsumingChildTerminalObservationV1,
        signal_to_send: Signal,
    ) -> Result<bool, OwnedChildReaperErrorV1> {
        match signal::killpg(Pid::from_raw(self.process_group_id), signal_to_send) {
            Ok(()) => Ok(true),
            // ESRCH is a no-op only after WNOWAIT has established that the direct child is
            // terminal. Darwin may report EPERM when the terminal leader's former PGID is no
            // longer signalable, even though an exact detached member still needs its own
            // cleanup. Neither case is a cleanup receipt: both defer proof to the post-reap
            // group-absence probe and the exact known-member checks below. Before terminal,
            // every signal error remains fail-closed.
            Err(Errno::ESRCH) if terminal == NonConsumingChildTerminalObservationV1::Terminal => {
                Ok(false)
            }
            Err(Errno::EPERM) if terminal == NonConsumingChildTerminalObservationV1::Terminal => {
                Ok(false)
            }
            Err(error) => Err(OwnedChildReaperErrorV1::ProcessGroupSignal {
                signal: signal_to_send as i32,
                error,
            }),
        }
    }

    fn signal_known_live_members(
        &self,
        signal_to_send: Signal,
    ) -> Result<(), OwnedChildReaperErrorV1> {
        for identity in self.known_members.values() {
            match observe_process_identity(identity.process_id()) {
                Ok(current) if current == *identity => {
                    let process_id = i32::try_from(identity.process_id()).map_err(|_| {
                        OwnedChildReaperErrorV1::InvalidProcessId(identity.process_id())
                    })?;
                    signal::kill(Pid::from_raw(process_id), signal_to_send).map_err(|error| {
                        OwnedChildReaperErrorV1::KnownMemberSignal {
                            process_id: identity.process_id(),
                            signal: signal_to_send as i32,
                            error,
                        }
                    })?;
                }
                Ok(_) => {
                    return Err(OwnedChildReaperErrorV1::KnownMemberBirthIdentityMismatch {
                        process_id: identity.process_id(),
                    });
                }
                Err(ProcessIdentityObservationErrorV1::Absent)
                | Err(ProcessIdentityObservationErrorV1::NotLive(_)) => {}
                Err(error) => {
                    return Err(OwnedChildReaperErrorV1::KnownMemberObservation {
                        process_id: identity.process_id(),
                        error,
                    });
                }
            }
        }
        Ok(())
    }

    fn known_members_are_terminal(&self) -> Result<bool, OwnedChildReaperErrorV1> {
        for identity in self.known_members.values() {
            match observe_process_identity(identity.process_id()) {
                Ok(current) if current == *identity => return Ok(false),
                Ok(_) => {
                    return Err(OwnedChildReaperErrorV1::KnownMemberBirthIdentityMismatch {
                        process_id: identity.process_id(),
                    });
                }
                Err(ProcessIdentityObservationErrorV1::Absent)
                | Err(ProcessIdentityObservationErrorV1::NotLive(_)) => {}
                Err(error) => {
                    return Err(OwnedChildReaperErrorV1::KnownMemberObservation {
                        process_id: identity.process_id(),
                        error,
                    });
                }
            }
        }
        Ok(true)
    }

    fn wait_for_direct_terminal_until(
        &mut self,
        deadline: Instant,
    ) -> Result<bool, OwnedChildReaperErrorV1> {
        loop {
            if self.observe_terminal_without_reap()?
                == NonConsumingChildTerminalObservationV1::Terminal
            {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            thread::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));
        }
    }

    fn wait_for_known_members_until(
        &self,
        deadline: Instant,
    ) -> Result<bool, OwnedChildReaperErrorV1> {
        loop {
            if self.known_members_are_terminal()? {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            thread::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));
        }
    }

    fn group_absent_after_reap_until(
        &self,
        deadline: Instant,
    ) -> Result<bool, OwnedChildReaperErrorV1> {
        loop {
            match signal::killpg(Pid::from_raw(self.process_group_id), None) {
                Err(Errno::ESRCH) => return Ok(true),
                Ok(()) => {
                    if Instant::now() >= deadline {
                        return Ok(false);
                    }
                }
                Err(error) => return Err(OwnedChildReaperErrorV1::ProcessGroupObservation(error)),
            }
            thread::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));
        }
    }
}

impl OwnedChildLifecycleV1 for UnixOwnedChildReaperV1 {
    fn observe_terminal_without_reap(
        &mut self,
    ) -> Result<NonConsumingChildTerminalObservationV1, OwnedChildReaperErrorV1> {
        self.ensure_not_reaped()?;
        observe_child_terminal_without_reap(self.direct_child_process_id())
    }

    fn cleanup_and_reap(
        &mut self,
        deadline: Instant,
        terminate_grace: Duration,
    ) -> Result<BoundedNativeCleanupReceiptV1, OwnedChildReaperErrorV1> {
        let result = self.cleanup_and_reap_inner(deadline, terminate_grace);
        if result.is_err() && !self.reaped {
            self.best_effort_reap_direct_child();
        }
        result
    }
}

impl UnixOwnedChildReaperV1 {
    fn cleanup_and_reap_inner(
        &mut self,
        deadline: Instant,
        terminate_grace: Duration,
    ) -> Result<BoundedNativeCleanupReceiptV1, OwnedChildReaperErrorV1> {
        if Instant::now() >= deadline {
            return Err(OwnedChildReaperErrorV1::DeadlineExceeded);
        }
        let mut terminal = self.ensure_direct_child_identity_or_terminal()?;
        let mut group_present = self.ensure_original_process_group(terminal)?;

        // While the direct child is live or retained as a zombie by WNOWAIT, its PID protects the
        // original process-group number from reuse. Signal that group before the sole reap.
        if group_present {
            group_present = self.signal_owned_group(terminal, Signal::SIGTERM)?;
        }
        self.signal_known_live_members(Signal::SIGTERM)?;
        let graceful_deadline = deadline.min(Instant::now() + terminate_grace);
        let direct_terminal = terminal == NonConsumingChildTerminalObservationV1::Terminal
            || self.wait_for_direct_terminal_until(graceful_deadline)?;
        let members_terminal = self.wait_for_known_members_until(graceful_deadline)?;

        if !direct_terminal || !members_terminal {
            terminal = self.ensure_direct_child_identity_or_terminal()?;
            if group_present {
                group_present = self.ensure_original_process_group(terminal)?;
                if group_present {
                    self.signal_owned_group(terminal, Signal::SIGKILL)?;
                }
            }
            self.signal_known_live_members(Signal::SIGKILL)?;
        }
        if !self.wait_for_direct_terminal_until(deadline)?
            || !self.wait_for_known_members_until(deadline)?
        {
            return Err(OwnedChildReaperErrorV1::DeadlineExceeded);
        }

        let status = self
            .child
            .wait()
            .map_err(OwnedChildReaperErrorV1::DirectChildReap)?;
        self.reaped = true;
        self.terminal_exit_status = Some(status);

        // After reap we only probe. A numeric PGID can now be reused, so a present group is an
        // unknown/incomplete cleanup fact, never a reason to send another signal.
        if !self.group_absent_after_reap_until(deadline)? {
            return Err(OwnedChildReaperErrorV1::ProcessGroupStillPresentAfterReap);
        }
        Ok(BoundedNativeCleanupReceiptV1 {
            direct_child_process_id: self.direct_child_process_id(),
            registered_member_count: self.known_members.len(),
        })
    }
}

impl UnixOwnedChildReaperV1 {
    fn best_effort_reap_direct_child(&mut self) {
        let _ = self.child.kill();
        if let Ok(status) = self.child.wait() {
            self.terminal_exit_status = Some(status);
            self.reaped = true;
        }
    }
}

fn observe_child_terminal_without_reap(
    process_id: u32,
) -> Result<NonConsumingChildTerminalObservationV1, OwnedChildReaperErrorV1> {
    let process_id = i32::try_from(process_id)
        .map_err(|_| OwnedChildReaperErrorV1::InvalidProcessId(process_id))?;
    // SAFETY: the caller owns the direct child handle and no raw pointer escapes this call.
    // `WNOWAIT` intentionally preserves its waitable status for the one later `Child::wait`,
    // while `WNOHANG` keeps every observation bounded.
    let mut info: nix::libc::siginfo_t = unsafe { std::mem::zeroed() };
    let result = unsafe {
        nix::libc::waitid(
            nix::libc::P_PID,
            process_id as nix::libc::id_t,
            &raw mut info,
            nix::libc::WEXITED | nix::libc::WNOHANG | nix::libc::WNOWAIT,
        )
    };
    if result != 0 {
        return Err(OwnedChildReaperErrorV1::WaitObservation(
            io::Error::last_os_error(),
        ));
    }
    // SAFETY: `waitid` either wrote a SIGCHLD result or left zeroed storage for WNOHANG. The
    // libc accessors are the platform-supported way to read the active union member.
    let observed_process_id = unsafe { info.si_pid() };
    if observed_process_id == 0 {
        return Ok(NonConsumingChildTerminalObservationV1::StillRunning);
    }
    if observed_process_id != process_id {
        return Err(OwnedChildReaperErrorV1::WaitObservationMismatch {
            expected: process_id,
            observed: observed_process_id,
        });
    }
    Ok(NonConsumingChildTerminalObservationV1::Terminal)
}

fn current_process_group(process_id: u32) -> Result<i32, Errno> {
    let process_id = i32::try_from(process_id).map_err(|_| Errno::EINVAL)?;
    // SAFETY: getpgid reads the kernel's process-group association for one scalar PID and does
    // not retain a pointer or modify process state.
    let process_group_id = unsafe { nix::libc::getpgid(process_id) };
    if process_group_id < 0 {
        return Err(Errno::last());
    }
    Ok(process_group_id)
}

/// Fail-closed errors for bounded native cleanup. None may be converted into a receipt.
#[derive(Debug, thiserror::Error)]
pub enum OwnedChildReaperErrorV1 {
    #[error("direct child process id {0} is invalid")]
    InvalidProcessId(u32),
    #[error("direct child birth identity is not observable: {0}")]
    DirectChildObservation(ProcessIdentityObservationErrorV1),
    #[error("direct child birth identity drifted before cleanup")]
    DirectChildBirthIdentityMismatch,
    #[error("direct child is already reaped")]
    AlreadyReaped,
    #[error("direct child cannot be registered as a detached member")]
    DirectChildCannotBeRegisteredAsMember,
    #[error("registered member process {0} is duplicated")]
    DuplicateKnownMember(u32),
    #[error("owned process group was not established: expected {expected}, observed {observed}")]
    ProcessGroupOwnershipUnproven { expected: u32, observed: i32 },
    #[error("owned process group drifted: expected {expected}, observed {observed}")]
    ProcessGroupDrift { expected: i32, observed: i32 },
    #[error("non-consuming child wait failed: {0}")]
    WaitObservation(io::Error),
    #[error("non-consuming child wait observed process {observed}, expected {expected}")]
    WaitObservationMismatch { expected: i32, observed: i32 },
    #[error("owned process group signal {signal} failed: {error}")]
    ProcessGroupSignal { signal: i32, error: Errno },
    #[error("owned process group observation failed: {0}")]
    ProcessGroupObservation(Errno),
    #[error("registered member {process_id} birth identity drifted")]
    KnownMemberBirthIdentityMismatch { process_id: u32 },
    #[error("registered member {process_id} was not observable: {error}")]
    KnownMemberObservation {
        process_id: u32,
        error: ProcessIdentityObservationErrorV1,
    },
    #[error("registered member {process_id} signal {signal} failed: {error}")]
    KnownMemberSignal {
        process_id: u32,
        signal: i32,
        error: Errno,
    },
    #[error("direct child reap failed: {0}")]
    DirectChildReap(io::Error),
    #[error("bounded cleanup deadline elapsed")]
    DeadlineExceeded,
    #[error("owned process group remained present after the direct child was reaped")]
    ProcessGroupStillPresentAfterReap,
}
