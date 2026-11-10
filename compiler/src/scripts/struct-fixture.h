// A C library whose functions pass structs both ways and hand back several
// values at once. src/scripts/golden.sh binds it as a C dependency and calls it
// from struct-fixture.jde, so each shape the shim packs into a record is held
// to recorded output on both backends.
#include <stdint.h>
typedef struct { int64_t w; int64_t h; double scale; char name[16]; } box_t;
void box_make(int64_t w, int64_t h, box_t* out);
double box_area(const box_t* b);
int box_grow(box_t* b, int64_t by);
int divmod(int64_t a, int64_t b, int64_t* q, int64_t* r);
typedef struct { int64_t x; int64_t y; } pt_t;
pt_t pt_unit(void);
