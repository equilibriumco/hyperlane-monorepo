import React, { SVGProps, memo } from 'react';

function _MidnightLogo(props: SVGProps<SVGSVGElement>) {
  return (
    <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 36 36" {...props}>
      <circle cx="17.56" cy="18.29" r="17.5" fill="#0a0a1e" />
      <path
        d="M17.5597 0.790039C7.88697 0.790039 0.0472412 8.62417 0.0472412 18.29C0.0472412 27.9559 7.88697 35.79 17.5597 35.79C27.2325 35.79 35.0722 27.9559 35.0722 18.29C35.0722 8.62417 27.2325 0.790039 17.5597 0.790039ZM17.5597 32.5623C9.68306 32.5623 3.27725 26.1586 3.27725 18.29C3.27725 10.4214 9.68306 4.01774 17.5597 4.01774C25.4364 4.01774 31.8422 10.4214 31.8422 18.29C31.8422 26.1586 25.434 32.5623 17.5597 32.5623Z"
        fill="#ffffff"
      />
      <path
        d="M19.2033 16.6482H15.9166V19.9325H19.2033V16.6482Z"
        fill="#ffffff"
      />
      <path
        d="M19.2033 11.4609H15.9166V14.7453H19.2033V11.4609Z"
        fill="#ffffff"
      />
      <path
        d="M19.2033 6.27539H15.9166V9.55972H19.2033V6.27539Z"
        fill="#ffffff"
      />
    </svg>
  );
}

export const MidnightLogo = memo(_MidnightLogo);
