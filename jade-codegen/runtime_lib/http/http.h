/* http/http.h — std::http runtime module ABI.
 *
 * Every verb returns an ObjHeader dict { status:int, body:str(TAINTED) } and
 * never raises (network/spawn failure → status 0, empty body). `headers` is an
 * ObjHeader dict of string→string (or NULL); `body` is the request body (POST/PUT). */
#ifndef JADE_RT_HTTP_H
#define JADE_RT_HTTP_H

/* Each returns a tagged ObjHeader dict word { status:int, body:str(TAINTED) }
 * (already boxed via jrt_box_ptr). `headers` is an ObjHeader dict of
 * string→string (or NULL). Never raises except on transport failure. */
jade_value_t jrt_http_get(const char* url, void* headers);
jade_value_t jrt_http_post(const char* url, const char* body, void* headers);
jade_value_t jrt_http_put(const char* url, const char* body, void* headers);
jade_value_t jrt_http_delete(const char* url, void* headers);
jade_value_t jrt_http_head(const char* url, void* headers);

#endif /* JADE_RT_HTTP_H */
