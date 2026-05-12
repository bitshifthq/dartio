import 'package:ptyx/ptyx.dart' show PtyTermMode;
import 'package:test/test.dart';

void main() {
  group('PtyTermMode', () {
    group('passwordLike', () {
      test('derives password entry state from canonical echo mode', () {
        final states = [
          const PtyTermMode(canonical: true, echo: false).passwordLike,
          const PtyTermMode(canonical: true, echo: true).passwordLike,
          const PtyTermMode(echo: false).passwordLike,
        ];

        expect(states, [true, false, null]);
      });
    });

    group('equality', () {
      test('compares all known mode fields', () {
        const mode = PtyTermMode(canonical: true, echo: false, signals: true);
        const same = PtyTermMode(canonical: true, echo: false, signals: true);
        const different = PtyTermMode(
          canonical: false,
          echo: false,
          signals: true,
        );

        final equality = (
          equal: mode == same,
          sameHash: mode.hashCode == same.hashCode,
          different: mode == different,
        );

        expect(equality, (equal: true, sameHash: true, different: false));
      });
    });
  });
}
