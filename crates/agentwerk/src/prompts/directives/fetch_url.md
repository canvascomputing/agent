<!-- What `fetch_url` says about an address it will not fetch or could not read. -->

## fetch_url_too_long
`url` is {length} characters, over the {limit} character limit. Fetch one page rather than a long generated address.

## fetch_url_scheme_missing
`url` names no scheme. Write the full address, starting with https://.

## fetch_url_scheme_unsupported
Scheme `{scheme}` cannot be fetched. Use http or https.

## fetch_url_credentials_present
`url` carries embedded credentials, which are never sent. Remove the part before the @.

## fetch_url_host_missing
`url` names no host. Write the full address, such as https://example.com/page.

## fetch_url_host_not_resolvable
`{host}` is not a publicly resolvable host name. Fetch a public address instead.

## fetch_url_too_many_redirects
The address redirected more than {limit} times and was not followed further. Fetch the final address directly.

## fetch_url_request_failed
The request failed: {error}. Check the address, or fetch a different one.

## fetch_url_body_not_read
The response body could not be read: {error}. Fetch the address again, or another one.

## fetch_url_response_too_large
The response is {bytes} bytes, over the {limit} byte limit, and was not read. Fetch a smaller page.

## fetch_url_redirect_location_missing
The address redirected without a Location header, so there is nowhere to follow. Fetch the final address directly.
