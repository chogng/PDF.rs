# Oracle derivation

This infrastructure case deliberately creates one exact parse result and known
Scene, Text, and RGBA disagreements. Expected counts follow directly from the
project-authored synthetic artifact constructors:

- four generation-zero objects and no parse diagnostic;
- one missing/different Scene command;
- one different horizontal-LTR text run;
- one RGBA pixel with two changed channels.

Authority is O1 analytic. The synthetic counterpart is not an external engine
observation and must not be promoted to an O3 renderer golden.
