//! Direct Unix PTY candidate used only for architecture comparison.
//!
//! This implements the same synchronous benchmark ABI as the Rust candidate.
//! It is intentionally isolated from the production package and does not
//! claim asynchronous backpressure or cross-platform lifecycle support.

const std = @import("std");
const c = @cImport({
    @cInclude("errno.h");
    @cInclude("fcntl.h");
    @cInclude("signal.h");
    @cInclude("stdint.h");
    @cInclude("stdlib.h");
    @cInclude("sys/ioctl.h");
    @cInclude("sys/types.h");
    @cInclude("sys/wait.h");
    @cInclude("termios.h");
    @cInclude("unistd.h");
    @cInclude("util.h");
});

const abi_version: u32 = 1;
const ok: i32 = 0;
const invalid_argument: i32 = 1;
const os_error: i32 = 2;
const stale_handle: i32 = 3;

const Session = struct {
    master: c_int,
    pid: c.pid_t,
    waited: bool,
    exit_code: i32,
};

const Entry = struct {
    generation: u32,
    session: Session,
};

var registry_mutex: std.atomic.Mutex = .unlocked;
var registry: ?std.AutoHashMap(u32, Entry) = null;
var next_slot = std.atomic.Value(u32).init(1);
var next_generation = std.atomic.Value(u32).init(1);
threadlocal var last_os_error: i32 = 0;

fn rememberErrno() void {
    last_os_error = c.__error().*;
}

fn makeHandle(slot: u32, generation: u32) u64 {
    return (@as(u64, generation) << 32) | @as(u64, slot);
}

fn handleSlot(handle: u64) u32 {
    return @truncate(handle);
}

fn handleGeneration(handle: u64) u32 {
    return @truncate(handle >> 32);
}

fn mapLocked() *std.AutoHashMap(u32, Entry) {
    if (registry == null) {
        registry = std.AutoHashMap(u32, Entry).init(std.heap.c_allocator);
    }
    return &registry.?;
}

fn lockRegistry() void {
    while (!registry_mutex.tryLock()) std.atomic.spinLoopHint();
}

fn lookup(handle: u64) ?Session {
    lockRegistry();
    defer registry_mutex.unlock();
    const entry = mapLocked().get(handleSlot(handle)) orelse return null;
    if (entry.generation != handleGeneration(handle)) return null;
    return entry.session;
}

fn update(handle: u64, session: Session) bool {
    lockRegistry();
    defer registry_mutex.unlock();
    const entry = mapLocked().getPtr(handleSlot(handle)) orelse return false;
    if (entry.generation != handleGeneration(handle)) return false;
    entry.session = session;
    return true;
}

export fn ptyx_candidate_abi() callconv(.c) u32 {
    return abi_version;
}

export fn ptyx_candidate_spawn(
    script_pointer: ?[*]const u8,
    script_length: usize,
    out_handle: ?*u64,
) callconv(.c) i32 {
    const source = script_pointer orelse return invalid_argument;
    const destination = out_handle orelse return invalid_argument;
    if (script_length == 0) return invalid_argument;

    const script = std.heap.c_allocator.allocSentinel(
        u8,
        script_length,
        0,
    ) catch return os_error;
    defer std.heap.c_allocator.free(script);
    @memcpy(script[0..script_length], source[0..script_length]);

    var master: c_int = -1;
    var slave: c_int = -1;
    var size: c.struct_winsize = .{
        .ws_row = 24,
        .ws_col = 80,
        .ws_xpixel = 0,
        .ws_ypixel = 0,
    };
    if (c.openpty(&master, &slave, null, null, &size) != 0) {
        rememberErrno();
        return os_error;
    }

    const pid = c.fork();
    if (pid < 0) {
        rememberErrno();
        _ = c.close(master);
        _ = c.close(slave);
        return os_error;
    }
    if (pid == 0) {
        _ = c.close(master);
        if (c.setsid() < 0 or
            c.ioctl(slave, c.TIOCSCTTY, @as(c_int, 0)) < 0 or
            c.dup2(slave, c.STDIN_FILENO) < 0 or
            c.dup2(slave, c.STDOUT_FILENO) < 0 or
            c.dup2(slave, c.STDERR_FILENO) < 0)
        {
            c._exit(126);
        }
        if (slave > c.STDERR_FILENO) _ = c.close(slave);
        const shell: [*:0]const u8 = "/bin/sh";
        const dash_c: [*:0]const u8 = "-c";
        _ = c.execl(
            shell,
            shell,
            dash_c,
            script.ptr,
            @as(?[*:0]const u8, null),
        );
        c._exit(127);
    }

    _ = c.close(slave);
    const flags = c.fcntl(master, c.F_GETFD);
    if (flags >= 0) _ = c.fcntl(master, c.F_SETFD, flags | c.FD_CLOEXEC);

    const slot = next_slot.fetchAdd(1, .monotonic);
    const generation = next_generation.fetchAdd(1, .monotonic);
    const candidate_handle = makeHandle(slot, generation);
    lockRegistry();
    const result = mapLocked().put(slot, .{
        .generation = generation,
        .session = .{
            .master = master,
            .pid = pid,
            .waited = false,
            .exit_code = 0,
        },
    });
    registry_mutex.unlock();
    result catch {
        _ = c.kill(-pid, c.SIGKILL);
        _ = c.kill(pid, c.SIGKILL);
        _ = c.close(master);
        _ = c.waitpid(pid, null, 0);
        return os_error;
    };
    destination.* = candidate_handle;
    return ok;
}

export fn ptyx_candidate_read(
    handle: u64,
    bytes: ?[*]u8,
    capacity: usize,
) callconv(.c) i64 {
    const buffer = bytes orelse return -invalid_argument;
    if (capacity == 0) return -invalid_argument;
    const session = lookup(handle) orelse return -stale_handle;
    while (true) {
        const result = c.read(session.master, buffer, capacity);
        if (result >= 0) return result;
        if (c.__error().* == c.EINTR) continue;
        if (c.__error().* == c.EIO) return 0;
        rememberErrno();
        return -os_error;
    }
}

export fn ptyx_candidate_write(
    handle: u64,
    bytes: ?[*]const u8,
    length: usize,
) callconv(.c) i64 {
    const buffer = bytes orelse return -invalid_argument;
    if (length == 0) return -invalid_argument;
    const session = lookup(handle) orelse return -stale_handle;
    while (true) {
        const result = c.write(session.master, buffer, length);
        if (result >= 0) return result;
        if (c.__error().* == c.EINTR) continue;
        rememberErrno();
        return -os_error;
    }
}

export fn ptyx_candidate_wait(
    handle: u64,
    out_exit_code: ?*i32,
) callconv(.c) i32 {
    const destination = out_exit_code orelse return invalid_argument;
    var session = lookup(handle) orelse return stale_handle;
    if (!session.waited) {
        var status: c_int = 0;
        while (true) {
            const result = c.waitpid(session.pid, &status, 0);
            if (result == session.pid) {
                session.waited = true;
                const signal = status & 0x7f;
                session.exit_code = if (signal == 0)
                    @intCast((status >> 8) & 0xff)
                else
                    @as(i32, @intCast(128 + signal));
                if (!update(handle, session)) return stale_handle;
                break;
            }
            if (result < 0 and c.__error().* == c.EINTR) continue;
            rememberErrno();
            return os_error;
        }
    }
    destination.* = session.exit_code;
    return ok;
}

export fn ptyx_candidate_close(handle: u64) callconv(.c) i32 {
    const slot = handleSlot(handle);
    const generation = handleGeneration(handle);
    lockRegistry();
    const entry = mapLocked().get(slot) orelse {
        registry_mutex.unlock();
        return stale_handle;
    };
    if (entry.generation != generation) {
        registry_mutex.unlock();
        return stale_handle;
    }
    _ = mapLocked().remove(slot);
    registry_mutex.unlock();

    _ = c.close(entry.session.master);
    if (!entry.session.waited) {
        _ = c.kill(-entry.session.pid, c.SIGHUP);
        _ = c.kill(entry.session.pid, c.SIGHUP);
        var status: c_int = 0;
        while (true) {
            const result = c.waitpid(entry.session.pid, &status, 0);
            if (result == entry.session.pid) break;
            if (result < 0 and c.__error().* == c.ECHILD) break;
            if (result < 0 and c.__error().* == c.EINTR) continue;
            rememberErrno();
            return os_error;
        }
    }
    return ok;
}

export fn ptyx_candidate_last_os_error() callconv(.c) i32 {
    return last_os_error;
}
