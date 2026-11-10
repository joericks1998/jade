// The library struct-fixture.h declares. See there.
#include "struct-fixture.h"
#include <string.h>
void box_make(int64_t w, int64_t h, box_t* out) { out->w = w; out->h = h; out->scale = 1.5; strcpy(out->name, "made"); }
double box_area(const box_t* b) { return (double)(b->w * b->h) * b->scale; }
int box_grow(box_t* b, int64_t by) { b->w += by; b->h += by; return (int)(b->w + b->h); }
int divmod(int64_t a, int64_t b, int64_t* q, int64_t* r) { if (b == 0) return -1; *q = a / b; *r = a % b; return 0; }
pt_t pt_unit(void) { pt_t p; p.x = 1; p.y = 2; return p; }
