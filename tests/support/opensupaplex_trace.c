/* Deterministic fixtures executed by the unmodified upstream state machines.
 * Build against the pinned OpenSupaplex checkout with generate_traces.py.
 * Only graphics.c's blit entry is instrumented; the harness sets level/input data.
 */
#define main upstream_main
#include "src/supaplex.c"
#undef main

/* Tick number included by graphics.c's trace hook before each opaque copy. */
int referenceTick;

/* Run one isolated scenario. A fresh process resets every upstream global. */
int main(int argc, char **argv)
{
    if (argc != 4) return 1;
    const char *mode = argv[1];
    int tile = atoi(argv[2]);
    int direction = atoi(argv[3]);
    int opposite[] = {0, 3, 4, 1, 2};
    int offsets[] = {0, -60, -1, 60, 1};
    int snap = strcmp(mode, "snap") == 0;
    int walk = strcmp(mode, "walk") == 0;
    int push = strcmp(mode, "push") == 0;
    int left = direction == UserInputLeft;
    int source, target;

    /* Hardware encloses both the isolated action and the open movement arena. */
    for (int i = 0; i < kLevelDataLength; ++i) {
        gCurrentLevelState[i].tile = LevelTileTypeHardware;
        gCurrentLevelState[i].state = 0;
    }
    if (snap || walk) {
        source = 3 * 60 + 4;
        target = source + offsets[direction];
        gCurrentLevelState[target].tile = tile;
        gNumberOfRemainingInfotrons = tile == LevelTileTypeInfotron;
    } else {
        for (int y = 1; y < 7; ++y)
            for (int x = 1; x < 9; ++x)
                gCurrentLevelState[y * 60 + x].tile = LevelTileTypeSpace;
        int start = left ? 5 : 3;
        int object = left ? 3 : 5;
        source = 2 * 60 + start;
        target = 2 * 60 + (push ? 4 : object);
        gCurrentLevelState[target].tile = tile;
        gCurrentLevelState[3 * 60 + 4].tile = LevelTileTypeHardware;
        gCurrentLevelState[3 * 60 + object].tile = LevelTileTypeChip;
        if (!push) gCurrentLevelState[2 * 60 + 4].tile = LevelTileTypeBase;
    }
    gCurrentLevelState[source].tile = LevelTileTypeMurphy;
    gMurphyLocation = source;
    gMurphyTileX = source % 60;
    gMurphyTileY = source / 60;
    gMurphyPositionX = gMurphyTileX * 16;
    gMurphyPositionY = gMurphyTileY * 16;

    /* Reversals revisit the eaten cell while the rounded actor keeps updating. */
    int ticks = snap ? 8 : walk ? 24 : push ? 48 : 40;
    for (referenceTick = 1; referenceTick <= ticks; ++referenceTick) {
        int reverse = walk ? referenceTick > 8 && referenceTick <= 16
                           : !snap && !push && referenceTick > 16 && referenceTick <= 24;
        gCurrentUserInput = snap ? direction + 4 : reverse ? opposite[direction] : direction;
        updateMovingObjects();
        if (snap) {
            printf("STATE %d %u %u\n", referenceTick,
                   gCurrentLevelState[target].tile, gNumberOfRemainingInfotrons);
        } else if (walk) {
            printf("STATE %d %d %u\n", referenceTick, gMurphyLocation,
                   gCurrentLevelState[gMurphyLocation].state);
        } else {
            printf("STATE %d M %d %d %02x", referenceTick,
                   gMurphyLocation % 60, gMurphyLocation / 60,
                   gCurrentLevelState[gMurphyLocation].state);
            if (push) {
                for (int i = 0; i < 1440; ++i)
                    if (gCurrentLevelState[i].tile == LevelTileTypeZonk)
                        printf(" Z %d %d %02x", i % 60, i / 60, gCurrentLevelState[i].state);
            }
            puts("");
        }
    }
}
