// ignore_for_file: type=lint
@DefaultAsset('package:ptyx/ptyx.dart')
library;

import 'dart:ffi';

@Native<Bool Function(Pointer<Void>)>(symbol: 'ptyi_init')
external bool controllerInit(Pointer<Void> apiData);

@Native<
  Uint64 Function(
    Pointer<Char>,
    Pointer<Pointer<Char>>,
    Size,
    Pointer<Pointer<Char>>,
    Size,
    Bool,
    Pointer<Char>,
    Uint32,
    Uint32,
    Uint32,
    Uint32,
    Size,
    Size,
    Int64,
    Int64,
  )
>(symbol: 'ptyi_spawn')
external int controllerSpawn(
  Pointer<Char> executable,
  Pointer<Pointer<Char>> arguments,
  int argumentCount,
  Pointer<Pointer<Char>> environment,
  int environmentCount,
  bool inheritEnvironment,
  Pointer<Char> cwd,
  int rows,
  int columns,
  int pixelWidth,
  int pixelHeight,
  int inputCapacity,
  int outputCapacity,
  int outputPort,
  int eventPort,
);

@Native<Uint64 Function(Uint64, Pointer<Uint8>, Size)>(symbol: 'ptyi_write')
external int controllerWrite(int handle, Pointer<Uint8> bytes, int length);

@Native<Bool Function(Uint64, Size)>(symbol: 'ptyi_credit_async')
external bool controllerCredit(int handle, int bytes);

@Native<Bool Function(Uint64, Bool)>(symbol: 'ptyi_pause')
external bool controllerPause(int handle, bool paused);

@Native<Int32 Function(Uint64, Size, Uint64)>(symbol: 'ptyi_wait_capacity')
external int controllerWaitCapacity(int handle, int required, int waiter);

@Native<Int32 Function(Uint64, Uint64, Uint64)>(symbol: 'ptyi_wait_flush')
external int controllerWaitFlush(int handle, int sequence, int waiter);

@Native<Bool Function(Uint64, Pointer<Int32>)>(symbol: 'ptyi_exit_status')
external bool controllerExitStatus(int handle, Pointer<Int32> status);

@Native<Int64 Function(Uint64)>(symbol: 'ptyi_pid')
external int controllerPid(int handle);

@Native<Bool Function(Uint64, Pointer<Uint32>)>(symbol: 'ptyi_size')
external bool controllerSize(int handle, Pointer<Uint32> values);

@Native<Bool Function(Uint64, Uint32, Uint32, Uint32, Uint32)>(
  symbol: 'ptyi_resize',
)
external bool controllerResize(
  int handle,
  int rows,
  int columns,
  int pixelWidth,
  int pixelHeight,
);

@Native<Int32 Function(Uint64)>(symbol: 'ptyi_mode')
external int controllerMode(int handle);

@Native<IntPtr Function(Uint64, Pointer<Uint8>, Size)>(symbol: 'ptyi_tty_name')
external int controllerTtyName(int handle, Pointer<Uint8> target, int capacity);

@Native<Bool Function(Uint64)>(symbol: 'ptyi_close')
external bool controllerClose(int handle);

@Native<Bool Function(Uint64)>(symbol: 'ptyi_destroy')
external bool controllerDestroy(int handle);
