import 'package:ptyx/ptyx.dart' show PtySize;
import 'package:test/test.dart';

void main() {
  group('PtySize', () {
    group('equality', () {
      test('compares all cell and pixel dimensions', () {
        const size = PtySize(
          rows: 24,
          columns: 80,
          pixelWidth: 800,
          pixelHeight: 600,
        );
        const same = PtySize(
          rows: 24,
          columns: 80,
          pixelWidth: 800,
          pixelHeight: 600,
        );
        const different = PtySize(
          rows: 24,
          columns: 81,
          pixelWidth: 800,
          pixelHeight: 600,
        );

        final equality = (
          equal: size == same,
          sameHash: size.hashCode == same.hashCode,
          different: size == different,
        );

        expect(equality, (equal: true, sameHash: true, different: false));
      });
    });
  });
}
