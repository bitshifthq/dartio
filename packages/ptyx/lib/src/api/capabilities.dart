part of 'api.dart';

/// Native features available for a [PtySession].
@immutable
final class PtyCapabilities {
  /// Whether signals can be delivered with Unix signal semantics.
  final bool signals;

  /// Whether the session owns a Unix terminal process group.
  final bool processGroups;

  /// Whether terminal-mode queries and change observation are available.
  final bool terminalModes;

  /// Whether [PtySession.ttyName] is available.
  final bool terminalName;

  /// Creates an immutable capability snapshot.
  const PtyCapabilities({
    required this.signals,
    required this.processGroups,
    required this.terminalModes,
    required this.terminalName,
  });
}
