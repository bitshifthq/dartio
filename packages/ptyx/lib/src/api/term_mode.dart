part of 'api.dart';

/// Terminal input mode flags observed from a pseudo terminal.
///
/// Terminal programs can change these flags while running. A hidden input
/// prompt, for example, usually disables echo. Each field is nullable because
/// some platforms cannot report every flag.
///
/// Example:
///
/// ```dart
/// final modes = session.modeChanges.listen((mode) {
///   if (mode.passwordLike == true) {
///     handleHiddenInput();
///   }
/// });
/// ```
@immutable
final class PtyTermMode {
  /// Whether canonical input processing is enabled.
  ///
  /// In canonical mode, input is usually delivered a line at a time.
  final bool? canonical;

  /// Whether typed input is echoed by the terminal.
  final bool? echo;

  /// Whether terminal signal generation is enabled.
  ///
  /// When enabled, terminal control characters such as Ctrl-C can signal the
  /// foreground process.
  final bool? signals;

  /// Creates terminal mode flags.
  const PtyTermMode({this.canonical, this.echo, this.signals});

  @override
  int get hashCode => Object.hash(canonical, echo, signals);

  /// Whether the mode resembles hidden input.
  ///
  /// Returns `true` when canonical input is enabled and echo is disabled.
  /// Returns `null` when either flag is unavailable.
  bool? get passwordLike {
    final canonical = this.canonical;
    final echo = this.echo;
    if (canonical == null || echo == null) return null;
    return canonical && !echo;
  }

  @override
  bool operator ==(Object other) {
    return other is PtyTermMode &&
        other.canonical == canonical &&
        other.echo == echo &&
        other.signals == signals;
  }
}
